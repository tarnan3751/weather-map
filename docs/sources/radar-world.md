# World radar sources: key-free, verified 2026-11-06

All facts below were checked by fetching on 2026-11-06 (about 18:15-18:35 UTC) with `User-Agent: weather-map-dev`.
Anything not fetched is marked UNVERIFIED. Samples are in
`/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/radar/`.

## 0. Summary

| Source | Usable | Values | Coverage | Res | Cadence / latency | Format |
|---|---|---|---|---|---|---|
| EUMETNET OPERA composite (open S3 bucket) | YES, no key | raw dBZ (DBZH, 5 min), rain rate mm/h (RATE, 15 min), accumulation (ACRR) | ~24 countries, Europe + Iceland, Malta | 1 km (DBZH), 2 km (RATE) | DBZH 5 min, lag ~4 min; RATE/ACRR 15 min, lag ~12 min | ODIM HDF5 (float64) or 2-band float32 GeoTIFF |
| DWD (Germany) | YES, no key | raw dBZ (hx 250 m, wn 1 km), rate mm/h (ry, yw 1 km), +2 h nowcasts (rv rain, wn dBZ) | Germany + margins | 250 m / 1 km | 5 min, lag 1-3 min | ODIM HDF5, uint16/uint32, deflate; nowcasts in tar |
| DPC (Italy) | YES, no key (undocumented-ish web API) | raw dBZ (VMI), rain rate mm/h (SRI) | Italy + seas | 1 km | 5 min, lag ~7 min | GeoTIFF float32 LZW, strips |
| JMA (Japan) nowcast tiles | YES, no key | coloured paletted PNG, 8 rate classes only | Japan | tiles z4-z10 | 5 min obs + 5 min steps to +1 h, lag ~1 min | PNG tile (8-bit palette) |
| SMHI (Sweden) | YES, no key | raw dBZ | Sweden | 2 km | 5 min, lag ~5 min | ODIM HDF5 uint8 |
| MeteoSwiss (Switzerland) STAC | YES, no key | rain rate/accumulation (PRECIP, CombiPrecip) | Switzerland | 1 km | 5 min | HDF5 (not inspected) |
| FMI (Finland) S3 | YES, no key, single-site only | dBZ per radar | per site | - | 5 min | GeoTIFF |
| BoM (Australia) | NO (coloured PNG only, restricted) | image | - | - | - | PNG |
| Austria GeoSphere | NO raw radar (only INCA / nowcast blend) | model precip | Austria | 1 km | 15 min | NetCDF |

Everything in Europe that OPERA lists is reachable through one decoder. Add DWD for Germany at higher resolution plus the 2 h nowcast, DPC for Italy, JMA for Japan.

---

## 1. Germany: DWD open data (`https://opendata.dwd.de/weather/radar/`)

Top level: `composite/ konrad3d/ mesocyclones/ radolan/ radvor/ sites/`.
Retention is about 2 days (577 files of 5 min). Directory listing is nginx autoindex (HTML, one line per file with mtime and size).
Each product has a `LATEST` alias that is overwritten on every cycle.
`Last-Modified` of the LATEST alias was 1-3 min after nominal time.

Today the old RADOLAN binary (ASCII header `RW...`) is NOT served. All `radolan/` products are now ODIM HDF5 (`...-bin.hdf5`).
All files are HDF5 superblock v0 (`\x89HDF\r\n\x1a\n`), `/dataset1/data1/data` is a single chunk covering the whole array (chunks == shape), filter = deflate only (no shuffle).

### Candidates

| Product | Path | Quantity / meaning | dtype | shape (rows x cols) | gain / offset | undetect / nodata | step | 
|---|---|---|---|---|---|---|---|
| **ry** | `radolan/ry/raa01-ry_10000-latest-dwd---bin.hdf5` | `RATE` mm/h, Z-R refined with steadiness correction, 5 min composite, **best "current rain rate"** | uint16 | 1200 x 1100 | 0.01 / 0 | 0 / 65535 | 5 min |
| yw | `radolan/yw/raa01-yw_10000-latest-dwd---bin.hdf5` | `RATE` mm/h, gauge-adjusted ("quasi-adjusted") | uint16 | 1200 x 1100 | 0.01 / 0 | 0 / 65535 | 5 min, lags ry by ~40 min (end time 17:35 when ry was 18:15) |
| rw | `radolan/rw/...` | `ACRR` 1 h accumulation, gauge adjusted | uint16 | 1200 x 1100 | 0.1 / 0 | 0 / 65535 | 1 h, ~25 min late |
| sf | `radolan/sf/` | 24 h accumulation | UNVERIFIED (not opened) | | | | 1 h, lag ~1 h |
| **hx** | `composite/hx/composite_hx_LATEST-hd5` | `DBZH` column max reflectivity ("top view"), 250 m, best "current reflectivity" | uint16 | 4800 x 4400 | 0.002929821616590115 / -64.00292982161659 | 0 / 65535 | 5 min |
| wn | `composite/wn/composite_wn__LATEST.tar` (note double underscore) | `DBZH` max reflectivity, 1 km, **tar of 25 files `_000`..`_120` (+0..+120 min nowcast, 5 min steps)** | uint16 | 1200 x 1100 | same as hx | 0 / 65535 | 5 min |
| **rv** | `composite/rv/composite_rv_LATEST.tar` | `ACRR` precip accumulated per 5 min ("RV" forecast), **tar of 25 files `_000`..`_120`** | uint32 | 1200 x 1100 | 0.001 / -0.001 | 0 / 4294967295 | 5 min, +2 h |
| dmax | `composite/dmax/composite_dmax_LATEST-hd5` | `DBZH`, maximum over the cycle, 250 m | uint16 | 4800 x 4400 | as hx | 0 / 65535 | 5 min, mtime ~4 min after |
| rs | `composite/rs/composite_rs_LATEST.tar` | `ACRR` 1 h accumulation forecast (rs), tar of 25 files, start 17:10 end 18:10 | uint32 | 1200 x 1100 | 0.001 / -0.001 | | 5 min |
| hymecng | `composite/hymecng/composite_HymecNG_LATEST_000-hd5` | `CLASS` hydrometeor classes (uint8, 11-entry legend) | uint8 | 1200 x 1100 | 1 / 0 | undetect 254 / nodata 255 | 5 min |
| vii | `composite/vii/composite_VII_latest-hd5` | vertically integrated ice; not opened | | | | | |
| pg, hg | empty directories today (0 files) | | | | | | |

Tar members are named `composite_rv_YYYYMMDD_HHMM_NNN-hd5` where NNN is lead minutes (000, 005 ... 120). The tar is uncompressed (mtime 12:12 local = 18:12 UTC).
The tar for rv is ~2.2 MB (25 x ~89 kB); a single HDF5 `ry` is 68 kB. The 250 m hx is 1 MB.

### Value decoding (verified)
- Reflectivity: `dBZ = raw * 0.002929821616590115 - 64.00292982161659`. Observed range -30.9 .. 64.4 dBZ (hx), -25.7 .. 64.4 (wn).
- RY / YW: `mm_per_h = raw * 0.01`. Observed max 47.38 mm/h.
- RW: `mm = raw * 0.1`.
- RV / RS: `mm_in_5min (or 1 h) = raw * 0.001 - 0.001`, so multiply by 12 for mm/h on RV. Note the offset is minus one gain step, so raw 1 means 0 mm. Observed max 3.378 mm/5 min = 40.5 mm/h.
- **Outside coverage vs no echo**: `nodata` (65535 / 4294967295) is outside the composite's valid area; `undetect` (0) is covered but no echo. In ry, 47.5 % of the rectangle is nodata, 50 % undetect, 2.5 % echo. In hx 47.2 % / 48.4 % / 4.4 %.

### Grid (verified, ODIM `/where`)
- RADOLAN-type grid (ry, yw, rw, wn, rv, rs, hymecng): `+proj=stere +lat_0=90 +lat_ts=60 +lon_0=10 +a=6378137 +b=6356752.3142451802 +units=m +x_0=543196.83521776402 +y_0=3622588.8619310018 +no_defs`, xsize 1100, ysize 1200, scale 1000 m.
  Outer corners: UL 55.862087N 1.463302E, UR 55.845439N 18.731616E, LL 45.696425N 3.566995E, LR 45.684606N 16.580869E.
- hx / dmax: same projection with `x_0=543571.83521776402 y_0=3622213.8619310022`, xsize 4400, ysize 4800, scale 250 m, same lat/lon corners.
- `x_0, y_0` are chosen so that the grid's **upper-left outer corner is projected (0, 0)**: x grows east in [0, xsize*xscale], y runs from 0 down to -ysize*yscale, row 0 is north. I checked this with a spherical stereographic formula (UL -> (1.7 km, 12 km) with sphere vs ellipsoid error; LR -> (1098 km, -1187 km)). Verify with real PROJ before shipping; PROJ is not importable in my venv (wheel mismatch), so exact corner agreement to the metre is UNVERIFIED.
- Cell (r, c) centre: `x = (c + 0.5) * xscale`, `y = -(r + 0.5) * yscale`.
- Other ODIM attributes: `Conventions ODIM_H5/V2_3`, `what/source ORG:78,CTY:616`, `how/nodes` lists 17 DWD sites, `how/missing_nodes` empty when complete.

### Nowcast
`composite/rv` (rate-like, +120 min) and `composite/wn` (dBZ, +120 min) carry it; `radvor/` has older `re` and `rq` products (UNVERIFIED, not opened).

### Licence
DWD open data; DWD states GeoNutzV, effectively CC BY 4.0 with source reference "Quelle: Deutscher Wetterdienst". UNVERIFIED today: the DWD copyright pages I tried returned 404 (`/EN/service/copyright/...`). No rule against automated access found; the server is intended for automated pull.

### Key gotchas
- Names use `-hd5` suffix with no `.h5` extension, `wn` LATEST has a double underscore.
- Retention only ~2 days. Pull by LATEST or compute the 5-min timestamp.
- uint32 for rv: use 64-bit math on the CPU or a float texture.
- hx is 21 M cells, so downsample on the server for tiling.

---

## 2. EUMETNET OPERA pan-European composite: key-free, via public S3 bucket

- Official route: MeteoGate "Open Radar Data (ORD)" API `https://api.meteogate.eu/eu-eumetnet-weather-radar/` (docs https://eumetnet.github.io/openradardata-documentation/). The collection listing returned 200 without credentials (`/collections?f=json`, bbox -22.6, 28.0, 29.8, 70.6, temporal interval rolling 24 h). Anonymous queries are rate limited (`x-ratelimit-remaining` header); an API key is available at the MeteoGate Developer Portal `https://devportal.meteogate.eu/` for a higher limit. We do not need it.
- **Direct bucket (no key at all)**: the ORD docs document an anonymous S3 bucket `openradar-24h` on CloudFerro (`https://s3.waw3-1.cloudferro.com/openradar-24h/`). Rolling 24 h cache, listing and GET both work anonymously over plain HTTPS.
  - Listing: `GET https://s3.waw3-1.cloudferro.com/openradar-24h?list-type=2&prefix=2026/11/06/OPERA/COMP/&max-keys=1000` returns S3 XML with `LastModified`.
  - Day folder also holds one folder per country (`BE CH CZ DE DK EE ES FI FR HR HU IE IS LT MT NL NO PL RO SE SK`, and `UK` presumably; not listed at 06 Nov but UK nodes are in the composite) with single-site ODIM volumes (`.../PVOL/`).
  - **Concrete 200 URLs**:
    - `https://s3.waw3-1.cloudferro.com/openradar-24h/2026/11/06/OPERA/COMP/OPERA@20261106T1815@0@DBZH.h5` (2.0 MB)
    - `.../OPERA@20261106T1815@0@DBZH.tiff` (2.3 MB)
    - `.../OPERA@20261106T1800@0@RATE.h5` (1.3 MB), `...RATE.tiff`, `...ACRR.h5`.
  - Pattern: `{YYYY}/{MM}/{DD}/OPERA/COMP/OPERA@{YYYYMMDD}T{HHMM}@0@{DBZH|RATE|ACRR}.{h5|tiff}`. The `@` must be percent-encoded as `%40` by strict clients (curl accepted raw `@`).
  - Discover newest: list the day prefix (paging 1000 keys; sort by name). Cheaper: the day prefix with `start-after=` set to the last known key.
- Cadence and latency (from `LastModified`): DBZH every 5 min, 220 files so far today, newest 18:15 object written 18:19:12 (lag 4 min). RATE and ACRR every 15 min (73 files), newest 18:00 written 18:12 (lag 12 min). Bucket also holds 24 h only; older data is the long-term archive via the API (not tested).
- Missing national nodes: no Austria (`at*`), Italy, Greece, Latvia, Bulgaria, Serbia, Turkey, Ukraine, Russia, Luxembourg listed in the DBZH node list; Spain only some sites; the footprint extends over the Atlantic but is empty there.

### Containers
**DBZH.h5** (superblock v0): `/dataset1/data1/data` float64, 4400 x 3800, chunks (760, 880), deflate only, **values are already physical** (gain 1, offset 0): dBZ. `nodata = -9999000` (outside coverage), `undetect = -8888000` (covered, no echo). Observed range -33.5 .. 80.5 dBZ; 49.8 % nodata, 44.4 % undetect, 5.9 % detected. Quality plane `/dataset1/data1/quality1/data` (float64, qi_total, 0..1).
Attributes: `what/product MAX`, prodname "OPERA CIRRUS maximum reflectivity composite", `how/license https://creativecommons.org/licenses/by/4.0/ (CC BY 4.0)`, `H5rad 2.4`.
`where/projdef = +proj=laea +lat_0=55.0 +lon_0=10.0 +x_0=1950000.0 +y_0=-2100000.0 +units=m +ellps=WGS84`, xsize 3800, ysize 4400, scale 1000 m. Corners UL 67.0228N -39.5358E, UR 67.6210N 57.8120E, LL 31.7462N -10.4346E, LR 31.9877N 29.4210E. As with DWD, the UL outer corner is projected (0, 0) (spherical LAEA check gave UL ~(6 km, -2 km), LR (3799 km, -4404 km)), i.e. x in [0, 3800 km], y in [-4400 km, 0].
Note: the `how/comment` says certain DBZH volume inputs are advected by Meteo-France (pysteps); so some sites' data can be extrapolated slightly.

**RATE.h5**: float64, 2200 x 1900, chunks (69, 119), deflate, 2 km scale, same LAEA but xscale 2000, same corner points. mm/h, observed 0.. 612.15 (outliers; clamp). Nodata/undetect as above. `zr_a 200, zr_b 1.6`. "OPERA NIMBUS instantaneous rain rate composite", creator GeoSphere Austria. 49 % nodata, 48 % undetect, 3 % rain.

**GeoTIFF (`*.tiff`)**: 2-band **float32**, chunky (PlanarConfiguration 1), tiled 512 x 512, compression 8 (Deflate, `Predictor` tag absent so none), no palette. Band 1 = value, band 2 = quality index (0-1). Georeferencing: `ModelPixelScale (1000,1000)` (2000 for RATE), `ModelTiepoint (0,0,0 -> -500.0003, 499.9999)`. GeoKeys: user-defined projection, `ProjCoordTrans=10` (Lambert Azimuthal Equal Area), natural origin lat 55 lon 10, false easting 1 950 000 / northing -2 100 000, ellipsoid a=6378137 inv f=298.257223563. Note the tiepoint is offset by half a cell (-500, +500) relative to the HDF5's (0,0) corner, which would mean a half-pixel shift between the two encodings (PixelIsArea vs corner semantics); I did not resolve this. GDAL_NODATA tag = `-9999000`. **In the TIFF, "undetect" is stored as NaN** (44.4 % NaN in DBZH) while "outside coverage" stays -9999000 (49.8 %). So the two classes ARE distinguishable in both formats.

Recommendation: use the HDF5 (physical values, quality plane optional) or the TIFF (Deflate, tiled, one decoder for anything GDAL-like). Either is float.

Licence: CC BY 4.0 (stated in file metadata and in the ORD docs; "with exceptions noted in metadata"). Attribution: "EUMETNET OPERA, contributing national meteorological services" (wording UNVERIFIED; check the MeteoGate terms page https://eumetnet.github.io/meteogate-documentation/). The bucket and API are intended for automated use.

---

## 3. Other European national sources

Because OPERA already includes these countries, national feeds are only worth adding for higher resolution or nowcast.

| Country | Key-free? | Notes |
|---|---|---|
| Germany | yes | section 1 |
| Sweden (SMHI) | yes | `https://opendata-download-radar.smhi.se/api/version/latest/area/sweden/product/comp.json` lists files; `.../comp/latest.h5` returned 200 (228 kB, mtime 18:30:23, valid time 18:25). ODIM H5rad 2.2, superblock v1, `/dataset1/data1/data` uint8 881 x 458, 2 km, deflate. `DBZH = raw*0.4 - 30`, nodata 255 (36 % today = outside), undetect 0 (64 %). `+proj=stere +ellps=bessel +lat_0=90 +lon_0=14 +lat_ts=60 +towgs84=0,0,0` with NO x_0/y_0: position from UL/UR/LL/LR lat/lon (UL 69.807N 5.324E, LR 53.707N 22.762E). Also quality planes 1-5. Licence not read (SMHI open data is CC BY 4.0; UNVERIFIED). |
| Switzerland (MeteoSwiss) | yes | STAC `https://data.geo.admin.ch/api/stac/v1/collections/ch.meteoschweiz.ogd-radar-precip/items?limit=1` (200, licence CC-BY). One item per day (`20261023-ch`) with hundreds of assets like `.../cpc2629600000_00060.001.h5` (CombiPrecip 60 min); PRECIP also. 14 days retention, items listed oldest first. Not opened (HDF5 content UNVERIFIED). Covered by OPERA (5 sites). |
| Finland (FMI) | yes (single-site only) | public S3 `https://fmi-opendata-radar-geotiff.s3-eu-west-1.amazonaws.com/` with keys `2026/11/06/fianj/202611060000_fianj_cappi_600_dbzh_finrad_qc.tif`; per-radar GeoTIFFs (CAPPI, echo top) every 5 min; no national composite seen. Contents not opened. In OPERA anyway. |
| Norway (MET) | probably | `https://thredds.met.no/thredds/catalog/remotesensing/reflectivity-nordic/catalog.html` returned 200 (NetCDF thredds). Not inspected. In OPERA. |
| Netherlands (KNMI) | **NO, key** | `api.dataplatform.knmi.nl/open-data/v1/datasets` -> 401 "Authorization field missing". KNMI Open Data API key sign-up: https://developer.dataplatform.knmi.nl/ (they publish a shared anonymous key, still a key). Netherlands is in OPERA. |
| France (Météo-France) | UNVERIFIED | `meteo.data.gouv.fr` is a JS single-page app and I found no key-free mosaic URL via the data.gouv.fr API (searches returned 0). Radar API on the Météo-France portal needs a key: https://portail-api.meteofrance.fr/. France (metropolitan) IS in OPERA. Overseas mosaics: UNVERIFIED. |
| Czechia (CHMI) | UNVERIFIED | `opendata.chmi.cz` did not resolve from here (DNS failure). In OPERA (czbrd, czska). |
| Denmark (DMI) | UNVERIFIED | `opendataapi.dmi.dk/v2/radardata` -> 404. In OPERA. |
| Austria (GeoSphere) | NO raw radar | `dataset.api.hub.geosphere.at/v1/datasets` lists INCA (1 h, 1 km), nowcast-v1-15min-1km (model, 8 variables incl. `rr`, grid EPSG:31287, bbox 45.5-49.5N 8.1-17.7E, forecast 13 steps) and no radar composite. A NetCDF fetch of the nowcast timed out at 120 s from here, UNVERIFIED. Not raw radar; Austria is partially covered by neighbouring OPERA sites. |
| UK (Met Office) | UNVERIFIED / key | DataHub requires a key (https://datahub.metoffice.gov.uk/). UK is in OPERA (ukcas, ukcle, ... 19 sites). |
| Italy (DPC) | **yes** | section 4 |
| Belgium, Ireland, Spain, Portugal, Poland, Slovakia, Hungary, Slovenia, Croatia, Estonia, Lithuania, Iceland, Romania | not individually checked | all are in the OPERA node list (be, ie, es, pt, pl, sk, hu, si, hr, ee, lt, is, ro). Not checked: AEMET, IPMA, IMGW national feeds (IMGW path guess returned 404). |
| Latvia, Greece, Turkey, Bulgaria, Serbia, Ukraine | not checked / not in OPERA | Latvia, Greece, Turkey have no nodes in OPERA; no key-free raw source found (not searched in depth). |

---

## 4. Italy: Dipartimento della Protezione Civile (DPC)

Web app `https://radar.protezionecivile.it/` uses a public backend (found by reading the site's JavaScript). Terms on the site: CC BY-SA 4.0 (link `creativecommons.org/licenses/by-sa/4.0/` in the page code). It is a web app API, not formally documented here, so it could change.

1. Latest time: `GET https://radar-api.protezionecivile.it/findLastProductByType?type=SRI&lang=it` -> `{"total":1,"lastProducts":[{"productType":"SRI","time":1793989200000,"period":"PT5M"}]}` (ms since epoch; this was 18:20:00 UTC when asked at ~18:28). Types seen in code: `VMI`, `SRI`, `SRT1`, `TEMP`, `HRD`. `type=VMI` also returned 200.
2. Download: `POST https://radar-api.protezionecivile.it/downloadProduct` body `{"productType":"SRI","productDate":1793989200000}` -> JSON with a presigned S3 URL, expiry 300 s, e.g. `https://s3-prod-dpc-radar.s3.eu-south-1.amazonaws.com/SRI/06-11-2026-18-20.tif?...`. GET returned 200: SRI 730 kB (Last-Modified 18:27:29), VMI 633 kB (18:27:27). So **latency is ~7 min** for the 18:20 product. Cadence 5 min.
3. Products:
   - **SRI** surface rainfall intensity, mm/h; **VMI** vertical maximum intensity, dBZ.
4. GeoTIFF: float32, 1200 x 1400, **LZW** (compression 5), strips of 1 row, no predictor, written by IDL 8.3, single band, no palette. ModelPixelScale (1000, 1000); ModelTiepoint (0,0 -> -600000, +650000). GeoKeys: ProjectedCSType absent, GeographicType 4326 (WGS84), `ProjCoordTrans=1` (GeoTIFF code 1 = Transverse Mercator), unit metre, GeoDoubleParams (12.5, 42.0) = natural origin longitude 12.5E, latitude 42.0N. UNVERIFIED: whether the geokeys really describe a Transverse Mercator (as written) or whether DPC's own documentation intends another projection; validate with a coastline overlay. Grid extent: x -600 km .. +600 km, y +650 km .. -750 km relative to (42N, 12.5E).
5. Values: SRI: -9999 = nodata (outside, 44 %), 0 = no rain (47.9 %), >0 rain (8.1 %), observed max 39.86 mm/h, smallest steps 0.01, 0.015, 0.02 ... (not a regular lattice). VMI: -9999 = nodata (93.7 % are -9999 or -9998), -9998 = below minimum (undetect), then 10, 10.5, 11 ... up to 45 dBZ (0.5 dB steps). The mapping of -9998 is inferred, UNVERIFIED against DPC documentation.
6. Sample coverage: Italy, Sicily, Sardinia, Tyrrhenian/Adriatic seas, parts of Corsica, Tunisia/Malta/Balkans coast.
7. Gotcha: each frame costs one POST + one presigned GET, URL valid 5 minutes. A WebSocket (`wss://radar-wss.protezionecivile.it/`, STOMP topic `/topic/product`) announces new products.

---

## 5. Japan: JMA nowcast tiles

Base: `https://www.jma.go.jp/bosai/jmatile/data/nowc/` (static files behind CloudFront, `cache-control: max-age=86400` on tiles).
- Time lists: `targetTimes_N1.json` (past, observed: `basetime == validtime`, 5 min steps, newest 18:20 at 18:21, Last-Modified 18:20:23), `targetTimes_N2.json` (forecast frames, basetime 18:20, validtime up to +60 min), `targetTimes_N3.json` (other elements: `thns`, `trns`, `slmcs_*`, 10 min steps; not inspected). Elements for N1/N2: `hrpns` (precip rate classes) and `hrpns_nd` (no-data mask, but URL attempts returned 404, UNVERIFIED how to address it).
- Tile URL: `{base}/{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png`, e.g. `https://www.jma.go.jp/bosai/jmatile/data/nowc/20261106182000/none/20261106182000/surf/hrpns/6/56/25.png` returned 200 (1484 bytes). Forecast: `.../20261106182000/none/20261106192000/surf/hrpns/8/227/100.png` returned 200 (empty tile). Standard Web Mercator XYZ, 256 px. Zoom 4..10 returned 200 (z4 tile 4/14/6 = 1559 bytes with rain). Empty tiles are a 334-byte transparent RGBA PNG.
- **Format: 8-bit palette PNG with 10 entries, `tRNS` transparency**; indexes 0 and 1 fully transparent (no rain), indexes 2-9 coloured. Colours: 2 = (242,242,255), 3 = (160,210,255), 4 = (33,140,255), 5 = (0,65,255), 6 = (250,245,0), 7 = (255,153,0), 8 = (255,40,0), 9 = (180,0,104). These correspond to JMA's published rain-rate class legend (<1, 1-5, 5-10, 10-20, 20-30, 30-50, 50-80, >=80 mm/h) from memory: class boundaries UNVERIFIED. The palette INDEX is the class, so decoding the index gives class not a raw rate; treat as 8 classes only.
- Not raw. No outside-coverage signal in the tile itself (transparent everywhere without rain; `hrpns_nd` presumably carries it).
- Coverage: Japan and surrounding seas (extent not measured).
- Terms: JMA site terms (https://www.jma.go.jp/jma/en/copyright.html) put content under Public Data License v1.0 (CC BY compatible). Requirements: cite the source ("Source: Japan Meteorological Agency website (URL)"), say when edited ("Based on ... (Japan Meteorological Agency)"), and do not present it as made by the Japanese government. `robots.txt` at that host returned an HTML page, not rules. The terms I read did not forbid automated access.

---

## 6. Rest of world: quick yes/no

| Country | Key-free raw values? | Notes |
|---|---|---|
| Japan | image classes only | section 5 |
| Australia (BoM) | NO | `ftp://ftp.bom.gov.au/anon/gen/radar/` listed anonymously (e.g. `IDR00004.T.202611061228.png`, 10-min coloured PNGs) but it is images only; BoM requires acknowledging its copyright/FTP terms; I could not locate the automated-access wording (UNVERIFIED), so treat as not usable. |
| Hong Kong (HKO) | UNVERIFIED | Open-data list names "Gridded rainfall nowcast in Hong Kong" (data.gov.hk); my guessed URL returned 404. Tiny area. |
| Singapore (NEA) | images only | `weather.gov.sg` rain-area radar pages are images; not fetched. Rain gauge JSON on `api.data.gov.sg` is keyless but not radar. |
| Korea (KMA), Taiwan (CWA), Thailand (TMD), Malaysia (MET) | NO, key or portal login | KMA API Hub https://apihub.kma.go.kr/ ; CWA https://opendata.cwa.gov.tw/ ; TMD https://data.tmd.go.th/ ; MET Malaysia https://api.met.gov.my/ . Landing pages only, not tested further. |
| Philippines, NZ, Brazil, Argentina, Colombia, Mexico, South Africa, Israel, Gulf | not found | Website radar images only as far as I looked (NZ MetService, SAWS, SMN, CONAGUA, IMS landing pages returned HTML); REDEMET (Brazil) needs a key (https://redemet.decea.mil.br/). No key-free raw source found. UNVERIFIED beyond that. |

---

## 7. Recommendation (decoder work per area/population)

1. **OPERA composite from the open S3 bucket.** One ODIM float decoder (or float32 GeoTIFF) covers ~24 countries and ~350 M people. DBZH every 5 min at 1 km; RATE (mm/h) every 15 min at 2 km. Needs a projection (LAEA) and handling of `-9999000` vs `-8888000` (HDF5) or NaN (TIFF).
2. **DWD** for Germany at 250 m (hx) / 1 km (ry rate), plus the 2 h nowcast (rv, wn tar). Same ODIM reader plus tar wrapper plus integer gain/offset, polar stereographic. Replaces the OPERA German cells.
3. **DPC Italy** (60 M people not in OPERA): GeoTIFF float32 LZW, 2 requests per frame, projection to confirm.
4. **JMA** (125 M people): easiest transport (XYZ tiles) but 8-class palette only. Treat as coloured, lowest quality; use only if no raw source appears.
5. **SMHI** and other national feeds: skip unless OPERA proves insufficient (Sweden already in OPERA at 1 km).

## 8. Key-gated or unavailable list

- KNMI (Netherlands): https://developer.dataplatform.knmi.nl/
- Météo-France APIs: https://portail-api.meteofrance.fr/
- Met Office DataHub (UK): https://datahub.metoffice.gov.uk/
- MeteoGate developer portal (optional for higher OPERA API limits): https://devportal.meteogate.eu/
- KMA (Korea): https://apihub.kma.go.kr/
- CWA (Taiwan): https://opendata.cwa.gov.tw/
- TMD (Thailand): https://data.tmd.go.th/
- MET Malaysia: https://api.met.gov.my/
- REDEMET (Brazil): https://redemet.decea.mil.br/

## 9. Samples saved

Under `/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/radar/`:
- `dwd/` raw downloads: `composite_hx_LATEST-hd5`, `composite_dmax_LATEST-hd5`, `composite_rv_LATEST.tar`, `composite_wn__LATEST.tar`, `composite_rs_LATEST.tar`, `composite_HymecNG_LATEST_000-hd5`, `raa01-ry_10000-latest-dwd---bin.hdf5`, `raa01-yw_...`, `raa01-rw_...`; `dwd/x/` extracted tar members.
- `opera/` `opera_DBZH.h5`, `opera_DBZH.tiff`, `opera_RATE.h5`, `opera_RATE.tiff`, `opera_ACRR.h5` (time stamps 18:15 for DBZH, 18:00 for RATE/ACRR).
- `dpc/` `dpc_SRI.tif`, `dpc_VMI.tif`, `resp_*.bin` (the presign JSON responses).
- `jma/` `t_*.png` tiles.
- `smhi/latest.h5`.
