# Wire protocol

What the browser and the server say to each other. JSON field names are
`snake_case`, times are RFC 3339 UTC strings, coordinates are WGS 84 degrees.
The Rust types named below (in `crates/wx-core`) are the source of truth for
the JSON shapes; this page is the map.

## Layers, sources, parts and frames

A **layer** (radar, temperature, wind, ...) has one **timeline** of **frames**,
running from the past straight into the future. A frame is one time step, and
it is a mosaic: a stack of **parts**, best on top. A part is one **source's**
data for that layer at one valid time ("HRRR's temperature for 17:00 from the
11:00 run", "the German radar composite at 12:05"). Where parts overlap, the
higher one wins, with a soft blend at its edge.

Which parts make up a frame is decided by the server
(`crates/wx-core/src/frame.rs` has the rules). In short: every source that has
data for that moment contributes its newest part; observations sit above
analyses, above fine regional models, above coarse global ones; a part can
stand in for a few later time steps when its source updates less often than
the layer ticks.

Tiles are rendered from a stack of parts, never from a frame as such. Two
frames that share the parts covering some tile share that tile, so when one
source updates, only the tiles it touches change.

### `GET /api/config`

```json
{
  "tile_url": "/tiles/{layer}/{z}/{x}/{y}/{parts}.wxt",
  "tile_cells": 256,
  "basemap_url": "/basemap/world.pmtiles",
  "vapid_public_key": "BASE64URL or null",
  "server_time": "2026-11-06T12:00:00Z"
}
```

`basemap_url` is a PMTiles archive (Protomaps basemap schema), or `null` when
none is configured. `vapid_public_key` is `null` when push is not configured.

### `GET /api/sources`

`{ "sources": [SourceDef, ...] }`: `wx_core::source::SourceDef` plus `medium`:

```json
{ "id": "hrrr", "family": "hrrr", "region": "conus", "title": "HRRR", "provider": "NOAA",
  "attribution": "NOAA/NCEP HRRR", "attribution_url": "https://www.noaa.gov/",
  "licence": "Public domain (US government work)",
  "kind": "forecast", "medium": "model", "rank": 600, "res_km": 3.0, "max_zoom": 6 }
```

`medium` is `radar | satellite | analysis | nowcast | model`: what the values
come from, for wording a reading (a radar "sees no echo"; a satellite
estimate "sees no rain").

The client must show the `attribution` of every source that has a part in
view, linked to `attribution_url` (the licence itself where the licence asks
for a link). `licence` names the licence the data is used under. `rank` is the
stacking order (higher on top).

### `GET /api/layers`

`{ "layers": [LayerSpec, ...] }` in menu order. `LayerSpec` is
`wx_core::layer::LayerSpec` serialized as is:

```json
{
  "id": "wind",
  "title": "Wind and gusts",
  "group": "wind",
  "description": "Wind ten metres above the ground, and peak gusts",
  "bands": [
    { "name": "u", "quantity": "speed", "unit": "m/s", "scale": 0.01, "offset": -150.0,
      "kind": "continuous", "aggregate": "mean" },
    { "name": "v", "...": "..." },
    { "name": "gust", "...": "..." }
  ],
  "render": { "kind": "wind", "u": 0, "v": 1, "gust": 2 },
  "palette": "wind",
  "inputs": [ { "family": "rtma", "sources": [], "hold_minutes": 170, "fill_past": false, "...": "..." } ],
  "retention_minutes": 720,
  "observed_tick_minutes": 60,
  "anchored": false
}
```

* `group`: `precipitation | temperature | wind | sky`.
* `bands[].quantity`: `temperature` (stored °C), `speed` (m/s), `reflectivity`
  (dBZ), `precip_rate` (mm/h), `precip_depth` (mm), `snow_depth` (mm),
  `percent`, `category`. The client converts to display units.
* `render.kind`: `scalar {band}`, `wind {u, v, gust}`, `precip_type {rate, ptype}`.
  Precipitation type codes: 0 none, 1 rain, 2 snow, 3 sleet, 4 freezing rain, 5 mix.
* `palette` names a palette defined in the web client.
* `inputs[].sources` lists the source ids an input is limited to; empty means
  every source of its `family`. What a source contributes to a layer is the
  input whose `family` is the source's and whose `sources` is empty or names
  it; there is at most one.
* The `satellite` layer stores infrared brightness temperature in °C
  (quantity `temperature`); colder is higher cloud.
* The `radar` layer stores "radar sees nothing here" as −30 dBZ; no-data means
  nothing is looking there.
* `anchored` layers are accumulations (snowfall): every frame counts from the
  same starting moment, which is the `run` of the frame's parts.

### `GET /api/frames` and `GET /api/frames/{layer}`

```json
{ "server_time": "…", "timelines": { "radar": Timeline, "temp": Timeline } }
{ "layer": "radar", "frames": [Frame, ...], "parts": [Part, ...] }
```

A `Timeline` is `{ "frames": [...], "parts": [...] }`. Frames are sorted by
`valid`, oldest first, and a frame is identified by its `valid`:

```json
{ "valid": "2026-11-06T17:00:00Z", "kind": "forecast",
  "parts": ["hrrr-26110611-360", "gfs-26110606-660"] }
```

`kind` is `observed` up to the newest observation on the layer and `forecast`
after it. `parts` are part ids, top first. `parts` at the top level describes
every part any frame names (`wx_core::Part`):

```json
{ "id": "hrrr-26110611-360", "source": "hrrr",
  "valid": "2026-11-06T17:00:00Z", "run": "2026-11-06T11:00:00Z", "kind": "forecast",
  "bounds": { "west": -134.13, "south": 21.12, "east": -60.88, "north": 52.63 },
  "solid": { "west": -122.0, "south": 26.0, "east": -72.0, "north": 47.0 },
  "max_zoom": 6 }
```

* `source` is a `SourceDef.id`; `run` is `null` for observations. A part's
  `valid` can be earlier than the frame's when it is standing in.
* `bounds` is the lon/lat box enclosing the part's grid. **`west > east` means
  the box crosses the antimeridian** (it runs east from `west` to 180 and on
  from −180 to `east`); `west = -180, east = 180` is every longitude.
* `solid`, when present, is a box everywhere inside which the part has data at
  full weight, so nothing beneath it shows.
* `max_zoom` is the deepest zoom the server renders this part at.

Both responses carry a weak `ETag` and `Cache-Control: no-cache`, and are
gzipped when the client accepts it; send the `ETag` back as `If-None-Match`
for a `304`. For `/api/frames` it stands for the timelines: `server_time`
is when the body was made.

## Tiles

`GET /tiles/{layer}/{z}/{x}/{y}/{parts}.wxt` (XYZ scheme, Web-Mercator), where
`{parts}` is the stack of part ids the tile is rendered from, top first,
joined by commas.

**Which parts to name.** For a frame's parts in order (top first) and a tile
with lon/lat bounds `T`: take each part whose `bounds` intersect `T` (sharing
only an edge does not count), and stop after the first part whose `solid` box
contains `T`. That list is the tile's stack. Intersection and containment must
honour boxes that cross the antimeridian. `wx_core::frame::tile_parts` is the
reference implementation. An empty stack means the frame has nothing there:
do not request anything.

**How deep to go.** A stack offers tiles for `0 <= z <= max(part.max_zoom)`
over the parts in it. So tile selection is a walk down the quadtree: at each
tile compute its stack; if the view wants more detail and the stack's maximum
zoom is deeper than this tile, descend into its four children; otherwise use
this tile and let the renderer magnify it. A child whose own stack does not
reach the child's zoom (it lies beside the deep part, not under it) is not
requested: draw it from its parent's tile. Tiles over open ocean stop early at
the global model's zoom while tiles over a radar go deep.

| Status | Meaning |
|---|---|
| 200 | A WXT1 tile, `application/octet-stream`, immutable and cacheable for good |
| 204 | The stack is valid but the tile holds no data |
| 404 | Unknown layer or part, a stack no frame has (or had in the last four minutes) for this tile, or `z` deeper than the stack offers |
| 503 | Too many tiles are being rendered right now; retry after the `Retry-After` seconds |

The same URL always returns the same bytes, and a tile is named by its
content, not by a frame: when a source updates, frames change but most tile
URLs do not, so caches (the browser's, a CDN's, the client's own texture
cache) keep working. A frame's stacks stay fetchable for four minutes after
the frame changes or leaves its timeline, so a client animating through it is
not cut off mid-loop.

The server tolerates rounding differences: a part whose box is within one
millionth of a degree of the tile's edge may be included or left out.

### `GET /api/provenance?layer=..&valid=..&lon=..&lat=..`

What a value on the map is made of. `valid` is a frame's `valid`.

```json
{ "layer": "radar", "valid": "2026-11-06T11:42:00Z",
  "parts": [ { "id": "mrms-2611061142", "source": "mrms", "share": 0.82 },
             { "id": "cmorph-2611061030", "source": "cmorph", "share": 0.18 } ] }
```

`parts` are the parts of that frame that contribute to the value at that
point, top first, each with its share of the blend (shares sum to 1; more than
one part appears only in the soft edge where a source hands over to the one
beneath). An empty list means there is no data there. 404 for an unknown layer
or a `valid` that is not a frame. The answer costs the server a look into the
parts' grids, so ask when the pointer rests or on a tap, not on every move; a
point inside the top part's `solid` box needs no request at all.

### WXT1 tile format

All integers little-endian. The Rust codec is `crates/wx-tile`.

```text
offset size field
     0    4 magic            "WXT1"
     4    2 width            samples per row (257)
     6    2 height           rows (257)
     8    1 bands            1..=8, same order as LayerSpec.bands
     9    1 compression      0 = stored, 1 = zlib (RFC 1950)
    10    1 predictor        0 = none, 1 = delta + byte planes
    11    1 reserved
    12    4 raw_len          payload length once decompressed = bands*width*height*2
    16    … payload
```

* A sample is a `u16`. **0 is no-data.** Otherwise
  `value = band.offset + (sample - 1) * band.scale` in the band's stored unit.
* **Samples sit on the tile's lattice points, not at cell centres.** A tile
  has 256 cells and 257 samples per edge: sample `(col, row)` is at fractional
  tile position `(col / 256, row / 256)`, row 0 on the north edge. Neighbouring
  tiles repeat each other's edge samples, so interpolating inside one tile is
  seamless and never needs a neighbour.
* Decompressed payload, per band in order, rows north to south, samples west
  to east:
  * predictor 0: the `u16` samples.
  * predictor 1: `width*height` low bytes then `width*height` high bytes of
    the *differences*. Each sample is stored as its wrapping (mod 65536)
    difference from the sample to its left; the first sample of a row is
    differenced against the first sample of the row above, and the very first
    sample against 0.

  To decode predictor 1 for one band:

  ```text
  above = 0
  for row in 0..height:
      prev = above
      for col in 0..width:
          i = row*width + col
          prev = (prev + (lo[i] | hi[i] << 8)) & 0xFFFF
          out[i] = prev
      above = out[row*width]
  ```

* Continuous bands are meant to be interpolated bilinearly (skipping no-data
  samples); categorical bands use the nearest sample.

## Live updates: `GET /ws`

A WebSocket. The server sends one JSON text message per event
(`wx_core::WsEvent`, tagged by `type`); the client sends nothing. After a
reconnect the client refetches `/api/frames` and `/api/alerts` to resync.

Every connection gets a `heartbeat` every 25 seconds, so a client that has
heard nothing for a minute should treat the connection as dead and reconnect.
The server closes a connection that has fallen too far behind with code 1013;
the client reconnects and resyncs as usual.

```json
{ "type": "hello", "server_time": "…" }
{ "type": "heartbeat", "server_time": "…" }
{ "type": "timeline", "layer": "radar", "parts": [Part], "frames": [Frame], "removed": ["2026-11-06T10:00:00Z"] }
{ "type": "alerts", "revision": 1793990045123, "added": ["id"], "updated": ["id"], "removed": ["id"] }
{ "type": "stations", "updated": "…", "count": 2481 }
{ "type": "feed", "feed": FeedHealth }
```

`timeline`: a layer's timeline changed. `frames` are new or changed time steps
(match them to what you hold by `valid`), `removed` are time steps that no
longer exist, and `parts` describes every part named by `frames` that the
timeline did not name before. A client keeps a per-layer dictionary of parts,
adds these to it, and forgets parts no frame names any more.

## Alerts

Official public alerts from two paths that share one table, one id space and
the endpoints below:

* **United States**: the National Weather Service active-alerts feed, every 30
  seconds. Shapes are the warning's own polygon or the union of the forecast
  zones / counties it names.
* **The rest of the world**: every source in the WMO register of alerting
  authorities (`severeweather.wmo.int/json/sources.json`) whose CAP feed can be
  read without an account or key, about 150 feeds, each polled about every 2
  minutes (MeteoAlarm every 4). The register's US entries are skipped. A
  hazard is a thread of CAP messages; only the newest message of a thread is
  active, so an Update replaces the alert it updates (it appears with a new
  id, the old id is removed) and a Cancel ends it. Messages join a thread
  when one references the other (within one issuer a reference also matches
  on the identifier alone, as some issuers refer to their messages under
  another sender address), when they reference the same earlier message
  (seen or not), or, for a message that references nothing stored, when it
  is from the same sender about the same event and area as an alert in
  effect at an overlapping time (a re-issue).
* **Which part of a message counts**: a message may hold several blocks in
  the language shown (one per area or time step). Every block in that
  language that is in effect counts: the shape and `area_desc` are those of
  all of them, `severity` (with its `urgency` and `certainty`) is the most
  severe one's, `expires` the latest. A block is not in effect when its
  urgency is `Past`, its response is `AllClear`, or it is a cancellation
  (headed "Cancellation of ..."). A message none of whose blocks is in
  effect ends its hazard, as a Cancel does.
* **Shapes** are the message's polygons and circles; alerts that name only
  NUTS 3 regions (several MeteoAlarm countries) get the union of those
  regions. Alerts that name only other region codes have no shape: they are
  listed for their country but not drawn, and never notify.
* **MeteoAlarm** feeds list only what is in force, so an alert that is no
  longer listed has ended, but only once it has been missing from two
  non-empty listings in a row in which every listed message has been read:
  an empty feed, a feed that cannot be read, or an update that could not be
  fetched yet never ends an alert.

Ids are opaque strings: NWS ids look like `urn:oid:2.49.0.1.840.0.…`, world
ids like `cap:1f0c9a3e5b7d2e4a6c80`. All text fields are plain text (tags and
control characters are removed on the way in): render them as text, never as
HTML. `web` links are always `http(s)`.

### The alert set's revision

The set of active alerts has a **revision**, a positive integer that rises
whenever what the map shows or what `/api/alerts` lists changes (an alert
without a shape starting or ending raises it too; the `alerts` event then
names no ids, since nothing on the map changed). It is announced in every
`alerts` event, in every `GET /api/alerts` response (`revision`) and in `tiles.json`.
Vector-tile URLs carry it, so a tile URL always means the same bytes and is
cached as immutable. When a client hears of a new revision it switches its
tile URLs to it (with MapLibre: `map.getSource('alerts').setTiles([url])`)
and refetches any alert list it shows.

A revision number never stands for two different sets of shapes, across
server restarts too: it is kept in the database, every new one is at least
the time of the change in milliseconds since 1970 (so a database restored
from a backup cannot bring an old number back), and a starting server takes
a new one before it serves anything. Revisions are therefore large
(`1793990045123`, within JavaScript's safe integers) and not consecutive:
compare them as numbers, newer is larger, and ignore an event whose revision
is not larger than the one you have.

### Vector tiles: `GET /api/alerts/tiles/{revision}/{z}/{x}/{y}.mvt`

Mapbox Vector Tile 2.1 (`Content-Type: application/vnd.mapbox-vector-tile`),
Web Mercator XYZ addressing, `z` 0 to 16, extent 4096, 64-unit buffer. Shapes
are simplified for the zoom and stop gaining detail at zoom 12, so a source
should set `maxzoom: 12` and overzoom beyond. A tile with nothing in it is an
empty `200` body.

* `{revision}` equal to the current revision: `Cache-Control: public,
  max-age=31536000, immutable`.
* An older revision: the current shapes, with `Cache-Control: no-cache` (a
  client that has not heard of the new revision yet still gets a correct
  map). Switch to the new revision.
* A revision newer than the current one, a tile address outside the zoom's
  grid, or a malformed path: `404`.

Every tile has one layer, **`alerts`**, of polygons, one feature per active
alert with a shape, with these properties:

| Property | Type | Meaning |
|---|---|---|
| `id` | string | Alert id, as in `/api/alerts/detail?id=` (use `promoteId: 'id'` for feature state) |
| `event` | string | Event name in the alert's shown language (`Tornado Warning`, `Yellow wind warning`, `Rain`) |
| `severity` | string | `Extreme`, `Severe`, `Moderate`, `Minor` or `Unknown` |
| `level` | integer | The colour key, always one of 4 (Extreme), 3 (Severe), 2 (Moderate), 1 (Minor), 0 (Unknown) |
| `category` | string | CAP category, lower case, always one of `met`, `geo`, `safety`, `security`, `rescue`, `fire`, `health`, `env`, `transport`, `infra`, `cbrne`, `other`. `other` also when the sender gave none or one CAP does not define (it is never absent or null) |
| `headline` | string | The headline (else the event), at most 120 characters |
| `expires` | integer | When it ends, Unix seconds: NWS `ends`, else `expires`; for a world alert whose sender gave no expiry, a day after its onset (see `expires_assumed`). Absent only for an NWS alert with neither |
| `expires_assumed` | boolean | `true` when `expires` is that assumed day, not the sender's; say so to the user ("until about …") |
| `source` | string | `us-nws` for NWS alerts, else the WMO register's source id (`de-dwd-en`) |
| `country` | string | ISO 3166-1 alpha-2 of the issuer, lower case: `us` for NWS alerts, the register's country for world ones (`de`); absent when the register gives none |
| `geom_source` | string | `polygon` or `zones` (NWS), `cap` or `nuts` (world); see below |

Features are written in ascending `level`, so in plain painting order the
most severe alert is on top; `fill-sort-key: ["get", "level"]` does the same
explicitly. A suggested palette follows MeteoAlarm's: level 4 red, 3 orange,
2 yellow, 1 green, 0 grey (NWS-style per-event colours can be keyed on
`event` where `source` is `us-nws`).

Shapes drawn from NUTS regions (`geom_source: "nuts"`) are © EuroGeographics
and licensed for non-commercial use with credit: while any is on screen the
map must show **"© EuroGeographics for the administrative boundaries"**
(`tiles.json` carries it as `attribution`; `/api/alerts/sources` lists it
under `attributions`).

```js
map.addSource('alerts', {
  type: 'vector',
  tiles: [`/api/alerts/tiles/${revision}/{z}/{x}/{y}.mvt`],
  maxzoom: 12,
  promoteId: 'id',
  attribution: tilejson.attribution,
});
map.addLayer({ id: 'alert-fill', type: 'fill', source: 'alerts', 'source-layer': 'alerts',
  paint: { 'fill-sort-key': ['get', 'level'],
           'fill-color': ['match', ['get', 'level'], 4, '#d7191c', 3, '#fd8d3c', 2, '#fecc5c', 1, '#78c679', '#9e9e9e'],
           'fill-opacity': 0.35 } });
```

### `GET /api/alerts/tiles.json`

[TileJSON 3.0.0](https://github.com/mapbox/tilejson-spec) for the current
revision (`Cache-Control: no-cache`):

```json
{ "tilejson": "3.0.0", "name": "alerts", "revision": 1793990045123,
  "tiles": ["/api/alerts/tiles/1793990045123/{z}/{x}/{y}.mvt"],
  "minzoom": 0, "maxzoom": 12,
  "attribution": "© EuroGeographics for the administrative boundaries",
  "vector_layers": [ { "id": "alerts", "minzoom": 0, "maxzoom": 12,
    "fields": { "id": "String", "event": "String", "severity": "String", "level": "Number",
                "category": "String", "headline": "String", "expires": "Number",
                "expires_assumed": "Boolean", "source": "String", "country": "String",
                "geom_source": "String" } } ] }
```

### `GET /api/alerts`

Active alerts as a GeoJSON `FeatureCollection`, most severe first, then newest.

| Parameter | Default | Meaning |
|---|---|---|
| `bbox` | none | `west,south,east,north` in degrees, `south` not above `north`; `west` greater than `east` crosses the antimeridian. Alerts whose shape meets the box (tested on the shape itself, so an alert split at the antimeridian is found from either side and not from the far side of the planet); with `unshaped=true` also alerts without a shape whose `anchor` is inside the box. |
| `country` | none | ISO 3166-1 alpha-2 (`it`, any case); only that country's alerts. |
| `unshaped` | `true` when `country` is given, else `false` (with or without `bbox`) | `true` adds alerts that have no shape (with `"geometry": null`); `false` leaves them out. |
| `limit` | 1000 | 1 to 5000. |

So: no parameters, or `bbox` alone, lists only alerts with a shape; `country`
alone lists all of that country's alerts, with or without a shape. For the
alerts relevant to a map view, send `bbox=<view>&unshaped=true`: the shaped
alerts in the view, plus the shapeless ones whose country point (`anchor`,
the register's one point per country) is in the view. That point can fall
outside a closely zoomed view of the same country, so to list a country's
shapeless alerts whatever the view, ask with `country=` (for instance for
the country of the place the user is looking at). NWS alerts still waiting
for their zones have no anchor and are never matched by `bbox`.

```json
{ "type": "FeatureCollection", "revision": 1793990045123, "truncated": false,
  "features": [
    { "type": "Feature", "id": "cap:1f0c9a3e5b7d2e4a6c80",
      "geometry": { "type": "MultiPolygon", "coordinates": [[[[ -65.6, 44.4 ], …]]] },
      "properties": {
        "id": "cap:1f0c9a3e5b7d2e4a6c80", "event": "weather", "severity": "Moderate",
        "urgency": "Future", "certainty": "Possible",
        "headline": "special weather statement in effect",
        "area_desc": "Annapolis County; Digby County; Kings County",
        "sent": "2026-11-06T08:58:22Z", "onset": "2026-11-06T08:58:22Z",
        "ends": null, "expires": "2026-11-07T00:58:22Z", "expires_assumed": false,
        "geom_source": "cap", "source": "ca-msc-xx", "country": "ca",
        "language": "en-CA", "category": "met",
        "anchor": [ -65.116263, 44.8113 ] } },
    { "type": "Feature", "id": "cap:8b2d0e6f4a1c9e3b5d77", "geometry": null,
      "properties": { "id": "cap:8b2d0e6f4a1c9e3b5d77", "event": "Yellow Thunderstorm Warning",
        "severity": "Moderate", "geom_source": "none", "source": "it-meteoam-it",
        "country": "it", "language": "en-GB", "anchor": [ 12.810059, 42.306769 ], "…": "…" } } ] }
```

* `geometry` is a light display shape (simplified, a few decimals), or `null`
  for an alert without a shape. Point queries use the full stored shape.
* `truncated` is `true` when more alerts matched than `limit`.
* `anchor` is `[lon, lat]`: a point on the shape, or for an alert without a
  shape the register's point for its country. It may be `null` for NWS
  alerts still waiting for their zones.
* Responses carry a weak `ETag` (answer `If-None-Match` with `304`), `Cache-Control:
  no-cache`, and are gzipped when the client accepts it.
* Bad parameters are `400` with `invalid_bbox`, `invalid_country`,
  `invalid_limit` or `invalid_query`.

Properties (shared by `/api/alerts/at` and the detail endpoint):

| Field | Meaning |
|---|---|
| `id`, `event`, `severity`, `headline`, `area_desc` | As in the tiles; `headline` and `area_desc` in full, may be `null` |
| `urgency` | `Immediate`, `Expected`, `Future`, `Past`, `Unknown` or `null` |
| `certainty` | `Observed`, `Likely`, `Possible`, `Unlikely`, `Unknown` or `null` |
| `sent`, `onset`, `ends` | RFC 3339 UTC or `null`. `ends` is NWS-only: when the hazard ends (`expires` is then when the next statement is due) |
| `expires` | RFC 3339 UTC. For NWS alerts as the NWS gives it (may be `null`). For world alerts never `null`: the sender's `expires`, or for a sender that gave none a day after the alert's onset (else effective, else sent), which is also when it leaves the map |
| `expires_assumed` | `true` when `expires` is that assumed day rather than the sender's own time; always `false` for NWS alerts |
| `geom_source` | `polygon` (the NWS warning's own polygon), `zones` (union of NWS zones), `cap` (the CAP message's polygons and circles), `nuts` (union of NUTS 3 regions), `none` (no shape) |
| `source`, `country` | As in the tiles: `us-nws` and `us` for NWS alerts; `country` is `null` when the register gives none |
| `language` | Language of the text shown (BCP 47, e.g. `en-GB`, `es-AR`): English when the message has it, else its first language |
| `category` | As in the tiles: always one of the twelve values, `other` when unknown |

### `GET /api/alerts/at?lat=..&lon=..`

`{ "alerts": [ {properties as above}, ... ] }`: the active alerts whose shape
covers the point, most severe first.

### `GET /api/alerts/detail?id=<alert id>`

The properties above plus `description`, `instruction`, `sender_name`,
`message_type` (`Alert`, `Update` or `Cancel`), `status`, `active`, the
issuer's `source_name` and `country_name`, and `infos`: every block of the
message, in every language (NWS alerts have one). In `infos`, `category` is
normalised as above, `onset` and `expires` are what the sender wrote in that
block (so `null` where the top-level `expires` is assumed), and `in_effect`
says whether the block counts (see "Which part of a message counts"). Alerts stay available here
for a day after they end (`active: false`), so a notification opened late
still has something to show. 404 if unknown.

The issuer, exactly:

| | `source` | `source_name` | `country` | `country_name` |
|---|---|---|---|---|
| NWS alert | `us-nws` | `United States of America: National Weather Service` | `us` | `United States of America` |
| World alert | register `sourceId` (`ca-msc-xx`) | register name, "Country: authority" (`Canada: Meteorological Service of Canada`) | register `authorityCountry`, lower case (`ca`), or `null` | register `countryName` (`Canada`), or `null` |

`source_name` and `country_name` appear only in the detail. For a world
source that has since left the register they come from the register bundled
with the server, and are `null` if it has never been listed there.

```json
{ "id": "cap:1f0c9a3e5b7d2e4a6c80", "event": "weather", "severity": "Moderate", "…": "…",
  "description": "Prolonged, heavy rainfall is possible. …", "instruction": null,
  "sender_name": "Environment Canada", "message_type": "Update", "status": "Actual",
  "active": true,
  "source_name": "Canada: Meteorological Service of Canada", "country_name": "Canada",
  "infos": [
    { "language": "en-CA", "event": "weather", "headline": "special weather statement in effect",
      "description": "…", "instruction": null, "area_desc": "Annapolis County; …",
      "sender_name": "Environment Canada", "web": "https://weather.gc.ca/",
      "category": "met", "severity": "Moderate", "urgency": "Future", "certainty": "Possible",
      "onset": "2026-11-06T08:58:22Z", "expires": "2026-11-07T00:58:22Z", "in_effect": true },
    { "language": "fr-CA", "event": "météo", "headline": "bulletin météorologique spécial en vigueur",
      "…": "…" } ] }
```

### `GET /api/alerts/sources`

The world path's health, failing sources first: what the status page shows
about it (the `alerts-world` feed's `detail` line summarises the same).

```json
{ "enabled": true,
  "register": { "loaded_from": "live", "fetched_at": "…", "last_error": null },
  "summary": { "sources": 154, "healthy": 98, "empty": 47, "failing": 9, "starting": 0 },
  "attributions": ["© EuroGeographics for the administrative boundaries"],
  "sources": [
    { "id": "kw-dom-en", "name": "Kuwait: Meteorological Department", "country": "kw", "country_name": "Kuwait",
      "region": 2, "feed": "https://www.met.gov.kw/rss_eng/kuwait_cap.xml",
      "state": "failing", "items": 0, "backlog": 0, "active_alerts": 0, "drawn_alerts": 0,
      "last_ok": null, "last_attempt": "…", "last_error": "feed: timed out",
      "consecutive_failures": 3 } ] }
```

`state` is `starting` (not polled yet), `healthy` (answered and lists
current items), `empty` (answered, nothing current) or `failing`. `region` is
the WMO Regional Association: 1 Africa, 2 Asia, 3 South America, 4 North and
Central America, 5 South-West Pacific, 6 Europe. `backlog` counts listed CAP
documents not fetched yet (they are fetched over the next polls, newest
first).

### Which alerts notify

A saved place with `notify` on gets one push per hazard: from the NWS, an
event whose name ends in "Warning"; from the rest of the world, CAP severity
`Severe` or `Extreme` with an urgency other than `Past`. Everything else is
drawn but silent. An update of a hazard notifies only places it newly covers,
or the places it covers when it first becomes serious enough. A world push's
`title` is the event and its `body` is `"<place name>: <the alert's own
headline>"`, both in the alert's shown language (see the push payload under
"Devices, saved places, push").

## Devices, saved places, push

No accounts. A browser registers once, stores the token, and sends
`Authorization: Bearer <token>` on every call below except registration.
401 means the token is unknown: register again.

| Call | Body | Reply |
|---|---|---|
| `POST /api/device` | none | `{ "device_id": "uuid", "token": "…" }` (200) |
| `GET /api/places` | | `{ "places": [Place, ...] }` |
| `POST /api/places` | `{ "name", "lat", "lon", "notify"? }` | `Place` (201). At most 20 per device (409 beyond). |
| `PATCH /api/places/{id}` | `{ "name"?, "notify"? }` | `Place` |
| `DELETE /api/places/{id}` | | 204 |
| `POST /api/push/subscription` | `{ "endpoint", "keys": { "p256dh", "auth" } }` (a `PushSubscription.toJSON()`) | 204 |
| `DELETE /api/push/subscription` | `{ "endpoint" }` | 204 |
| `POST /api/push/test` | none | `{ "sent": 1, "failed": 0 }`; 503 `push_not_configured` when the server has no VAPID keys |

```json
Place = { "id": "uuid", "name": "Home", "lat": 35.22, "lon": -97.44, "notify": true,
          "created_at": "…",
          "alerts": [ { "id": "…", "event": "…", "severity": "…", "headline": "…", "ends": "…" } ] }
```

`alerts` lists the active alerts currently covering the place. A device keeps
at most 5 push subscriptions; adding a sixth drops the oldest.

Validation failures are 400 (422 for a wrong JSON type) with one of
`invalid_body`, `invalid_query`, `invalid_coordinates`, `invalid_name`,
`invalid_subscription`; a 21st place is 409 `too_many_places`.

A push message's payload (what the service worker receives) is JSON:

```json
{ "title": "Tornado Warning", "body": "Home: Tornado Warning until 6:45 PM CST",
  "tag": "<event key>", "url": "/?alert=<alert id>&lat=35.22&lon=-97.44",
  "alert_id": "…", "place_id": "uuid", "severity": "Extreme" }
```

A push is sent when a serious alert first covers a place with `notify` on,
once per hazard per place however often its issuer updates it. Which alerts
count as serious is under "Which alerts notify" above: from the NWS, events
whose name ends in "Warning"; from every other issuer, CAP severity Severe or
Extreme.

## Stations

### `GET /api/stations`

Latest surface observations worldwide, as a GeoJSON `FeatureCollection`, one
point per station, from three independent feeds: airport weather reports
(METAR, about 5,000 stations), NOAA buoys and coastal stations (about 850) and
the Japan Meteorological Agency's AMeDAS network (about 1,300). A feed that is
down is simply missing from the answer; the others are unaffected. Stations
whose last report is more than three hours old are left out.

Query parameters, both optional:

| Parameter | Meaning |
|---|---|
| `bbox=west,south,east,north` | Only stations inside this box, in degrees. `west` greater than `east` means the box crosses the antimeridian (`170,50,-170,65` is the Bering Sea). Longitudes beyond +-180, as a map panned across the date line reports them, are wrapped; a box 360 degrees wide or more is the whole world. `south` must not exceed `north`. |
| `raw=1` | Include the raw METAR text (`raw`) for airports. Left out by default to keep the answer small. |

Without `bbox` the answer is every station (about 7,300 features, 2.4 MB
before gzip), so a client should send the box it is showing. `400
{ "error": "bad_request" }` for a malformed `bbox`. Each distinct query has its
own `ETag`; send it back as `If-None-Match` for a `304`.

```json
{ "type": "FeatureCollection",
  "sources": {
    "metar":  { "name": "Airport weather reports (METAR)",
                "attribution": "Source: Aviation Weather Center (NOAA/NWS), aviationweather.gov" },
    "ndbc":   { "name": "NOAA National Data Buoy Center",
                "attribution": "Source: NOAA National Data Buoy Center, ndbc.noaa.gov" },
    "amedas": { "name": "Japan Meteorological Agency AMeDAS",
                "attribution": "Source: Japan Meteorological Agency website (jma.go.jp)" } },
  "features": [
    { "type": "Feature", "id": "KOUN",
      "geometry": { "type": "Point", "coordinates": [-97.471, 35.244] },
      "properties": {
        "id": "KOUN", "name": "Norman/Univ Oklahoma Arpt, OK",
        "kind": "airport", "source": "metar",
        "observed": "2026-11-06T11:55:00Z",
        "temp_c": 8.3, "dewpoint_c": 7.3,
        "wind_dir_deg": 360, "wind_speed_ms": 5.1,
        "visibility_m": 16093, "altimeter_hpa": 1020.3,
        "flight_category": "VFR" } },
    { "type": "Feature", "id": "ndbc:41013",
      "geometry": { "type": "Point", "coordinates": [-77.764, 33.436] },
      "properties": {
        "id": "ndbc:41013", "name": "Buoy 41013", "kind": "buoy", "source": "ndbc",
        "observed": "2026-11-06T17:50:00Z",
        "temp_c": 24.2, "dewpoint_c": 22.2,
        "wind_dir_deg": 170, "wind_speed_ms": 7.0, "wind_gust_ms": 9.0,
        "altimeter_hpa": 1011.4, "wave_height_m": 1.2 } },
    { "type": "Feature", "id": "amedas:11016",
      "geometry": { "type": "Point", "coordinates": [141.678, 45.415] },
      "properties": {
        "id": "amedas:11016", "name": "Wakkanai", "kind": "station", "source": "amedas",
        "observed": "2026-11-06T18:10:00Z",
        "temp_c": -1.2, "wind_dir_deg": 225, "wind_speed_ms": 12.4,
        "visibility_m": 280, "altimeter_hpa": 996.6 } } ] }
```

Fields:

- `id`: unique across feeds. Airports use their ICAO code; the other feeds are
  prefixed with the feed (`ndbc:`, `amedas:`).
- `kind`: `airport`, `buoy` (moored buoys) or `station` (coastal and pier
  stations, and the AMeDAS network). Choose the symbol from this.
- `source`: a key of the top-level `sources` object, which holds the name and
  the attribution the client should show for it. `sources` lists only the feeds
  that have a station in this answer.
- `name`: a place name, or `Buoy <id>` / `Station <id>` where the feed has none.
- `observed`: when the station reported, UTC.
- Units are fixed: `temp_c`, `dewpoint_c`, `water_temp_c` in degrees Celsius,
  `wind_speed_ms` and `wind_gust_ms` in m/s, `wind_dir_deg` the direction the
  wind blows from in degrees (1 to 360), `visibility_m` in metres,
  `wave_height_m` in metres, `altimeter_hpa` the pressure at sea level in hPa
  (the altimeter setting for airports).
- A measurement a station does not report is absent, not `null`.
  `wind_dir_deg` is absent for calm or variable wind. `water_temp_c` and
  `wave_height_m` appear for buoys only, `flight_category` (`VFR`, `MVFR`,
  `IFR`, `LIFR`) and `raw` for airports only.
- Coordinates are rounded to 0.001 degree.

## Point forecast

### `GET /api/point?lat=..&lon=..`

The forecast for one spot, in one shape whatever its source. Anywhere the
National Weather Service covers (the 50 states and US territories) it comes
from the NWS; everywhere else from MET Norway. If those fail for the point, the
server's own model answers when it has one. All three are described by the
`source` field, so a client never needs to care.

```json
{ "lat": -1.2921, "lon": 36.822,
  "source": "met-no",
  "attribution": "Based on data from MET Norway",
  "place": null, "time_zone": null,
  "generated_at": "2026-11-06T17:18:29Z",
  "hourly": [
    { "time": "2026-11-06T18:00:00Z", "span_h": 1,
      "temp_c": 16.9, "pop": null, "precip_mm": 0.0,
      "wind_ms": 5.1, "wind_dir_deg": 22,
      "condition": "cloudy", "text": "Cloudy" },
    { "time": "2026-11-09T06:00:00Z", "span_h": 6,
      "temp_c": 19.6, "pop": null, "precip_mm": 1.4,
      "wind_ms": 2.2, "wind_dir_deg": 49,
      "condition": "lightrainshowers_day", "text": "Light rain showers" } ],
  "daily": [
    { "date": "2026-11-07", "low_c": 15.5, "high_c": 24.2,
      "pop": null, "precip_mm": 2.6,
      "condition": "lightrain", "text": null } ] }
```

The same for a US point:

```json
{ "lat": 35.2226, "lon": -97.4395,
  "source": "nws",
  "attribution": "Forecast from the US National Weather Service",
  "place": "Norman, OK", "time_zone": "America/Chicago",
  "generated_at": "2026-11-06T12:12:15Z",
  "hourly": [
    { "time": "2026-11-06T12:00:00Z", "span_h": 1,
      "temp_c": 9.0, "pop": 0, "precip_mm": null,
      "wind_ms": 3.6, "wind_dir_deg": 0,
      "condition": "fog", "text": "Patchy Fog" } ],
  "daily": [
    { "date": "2026-11-06", "low_c": 5.0, "high_c": 13.9,
      "pop": 0, "precip_mm": null, "condition": "fog",
      "text": "Overnight: Patchy fog. Mostly clear. Low around 45 ... Friday: ..." } ] }
```

Top level:

- `lat`, `lon`: the request, rounded to 4 decimals.
- `source`: `nws`, `met-no`, or `model` (or another name the server's own
  model sets). `attribution`: the credit line to show with the forecast;
  MET Norway's licence requires "Based on data from MET Norway".
- `place`, `time_zone`: only the NWS provides them (`"Norman, OK"`, an IANA
  zone name); otherwise `null`.
- `generated_at`: when the provider made the forecast, UTC. Show it.

`hourly`, in time order, entries already over are dropped:

- `time`: start of the step, UTC. `span_h`: how long the step lasts. It is `1`
  for hourly data; MET Norway gives `6` further out (about 2.5 days on), when
  `precip_mm`, `pop` and `condition` describe the whole six hours. The NWS is
  always `1` and runs about 156 hours; MET Norway about 9 days.
- `temp_c` degrees Celsius; `wind_ms` m/s; `wind_dir_deg` the direction the
  wind blows from, 0 to 359 (`null` where the provider has none).
- `pop`: chance of precipitation in percent. The NWS has it; MET Norway only
  inside the Nordic region, so expect `null` elsewhere.
- `precip_mm`: expected precipitation over the step in millimetres. MET Norway
  has it; the NWS hourly forecast does not (`null`).
- `condition`: a [MET Norway symbol code](https://api.met.no/weatherapi/weathericon/2.0/documentation),
  with a `_day`, `_night` or `_polartwilight` suffix for the codes that have
  one. NWS wording is mapped to the nearest code (`null` if it has no match).
  The codes used: `clearsky`, `fair`, `partlycloudy`, `cloudy`, `fog`, and for
  precipitation `[light|heavy]` + `rain`, `sleet` or `snow`, optionally +
  `showers`, optionally + `andthunder` (for example `lightrainshowers_day`,
  `heavysnow`, `rainandthunder`). A client must tolerate codes it does not know.
- `text`: the condition in English (`"Light rain showers"`; for the NWS its own
  short forecast, `"Patchy Fog"`).

`daily`, one entry per local calendar day, entries already over dropped, up to
about 7 days (NWS) or 10 (MET Norway):

- `date`: the local date, `YYYY-MM-DD`. For MET Norway, which gives no time
  zone, the local day is approximated from longitude (solar time).
- `low_c`, `high_c`: degrees Celsius. A day may lack one (the NWS "Tonight").
- `pop` (percent, the day's highest) and `precip_mm` (the day's total) as for
  `hourly`; either may be `null`.
- `condition`: a symbol code as above without a day/night suffix, for the
  middle of the day.
- `text`: the provider's own written forecast when it has one (the NWS: the
  period texts joined, each prefixed with its name), otherwise `null`. It is
  the provider's wording and may use its units (the NWS writes miles per hour
  and degrees Fahrenheit); format the numeric fields instead.

Any number may be `null`. Responses may be cached for a minute
(`Cache-Control: public, max-age=60`); the server caches each provider's
answer more aggressively (MET Norway data is refreshed no oftener than its
`Expires` allows, and at least 3 hours apart outside the Nordic region).

Errors: `400 { "error": "bad_request" }` for bad coordinates; `502 { "error":
"upstream" }` when no provider answered. There is no coverage error: MET
Norway forecasts every location.

## Status

### `GET /api/status`

```json
{ "server_time": "…", "started_at": "…", "clients": 3,
  "feeds": [ { ...FeedHealth, "stale": false } ],
  "layers": [ { "id": "radar", "frames": 61, "oldest_valid": "…", "newest_valid": "…",
                "newest_observed": "… or null" } ],
  "sources": [ { "id": "hrrr", "title": "HRRR", "provider": "NOAA", "parts": 233, "layers": 8,
                 "newest_valid": "… or null", "newest_run": "… or null" } ] }
```

`traffic` is what the feeds have downloaded since the server started, per
upstream host, largest first: `[{ "host": "noaa-hrrr-bdp-pds.s3.amazonaws.com",
"requests": 5120, "not_found": 3, "retries": 0, "bytes": 812345678 }]`.

`layers` and `sources` list every layer and every source in registry order,
including ones holding nothing. For a source, `parts` counts the parts it
holds across all layers, `layers` the layers it has a part on, `newest_valid`
its newest data time and `newest_run` its newest model run (`null` for
observations). A source with `parts: 0` is not delivering.

`FeedHealth` is `wx_core::FeedHealth`: `name`, `state`
(`starting | healthy | backing_off | stopped`), `expected_interval_secs`,
`started_at`, `last_success`, `last_data_time`, `detail`, `last_error`,
`last_error_at`, `consecutive_failures`, `restarts`, `next_retry_at`.

`GET /healthz` answers `200 ok` when the server is up.

## Errors

Non-2xx JSON replies are `{ "error": "machine_code", "message": "for humans" }`.
