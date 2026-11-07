# Worldwide alerts, point forecasts and station observations: verified sources

Research date: 2026-11-06. Every URL marked OK returned HTTP 200 on that day with `User-Agent: weather-map-dev`.
Anything not fetched is marked UNVERIFIED. api.weather.gov was NOT called. Samples are in
`/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/alerts-points-obs/`
(`feeds/`, `cap/`, `met/`, `obs/`, `obs2/`, `ca/`, `jp/`, `ma/`, `om/`; `analysis.json` / `analysis.txt` hold per-source results; `sources.json` is the WMO list).

## A. Alerts

### A1. WMO Severe Weather Information Centre / Alert Hub

- Source list (OK): `https://severeweather.wmo.int/json/sources.json` (135 KB JSON, `{"sources":[{"source":{...}}]}`).
  The list is the one the site's own "sources.html" page loads. It supports `If-Modified-Since` (returns 304; no ETag header was sent).
  Pages `https://severeweather.wmo.int/v2/` and `/v2/cap-sources/` return 403/404, so use the JSON.
- Count: **156 sources, 151 distinct countries/territories**. All 156 have `sourceIsOfficial: true` and `capAlertFeedStatus: "operating"`.
  Region ids: 1 Africa 48, 6 Europe 43, 2 Asia 24, 4 N/C America 17, 5 SW Pacific 11, 3 S America 9, plus a few multi-region.
- Per source fields: `sourceId`, `capAlertFeed` (one or several comma-separated URLs), `countryName`, `region`, `latlng`, `guid` (OID), `byLanguage[].name`, `metLink`.
- Where the feeds live (host of `capAlertFeed`):
  - `cap-sources.s3.amazonaws.com/<sourceId>/rss.xml` : 43 sources. The Alert Hub's own S3 re-hosting (the bucket in the brief, `alert-hub-sources`, returns 403; the right one is `cap-sources`). RSS 2.0, each `<item><link>` is a CAP XML file in the same bucket, `<guid>` is the CAP identifier. S3 gives `ETag` and `Last-Modified`; `If-None-Match` and `If-Modified-Since` both return 304 (tested). These mirror feeds keep OLD items (e.g. Botswana still listed an alert from 2026-10-13), so filter by `<expires>` in the CAP.
  - `feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-<country>` : 33 sources (Europe, see A2).
  - 4 `meteoalert.meteoinfo.ru` (Roshydromet CIS), 2 `api.weather.gov` (US land and marine; not touched), 2 `severeweather.wmo.int/v2/cap-alerts/<id>/rss.xml` (Australia, China, WMO-hosted), and about 70 national servers (DWD, FMI, Met Eireann, AEMET, SMN Mexico, INMET Brazil, MetService NZ, etc.).
- Formats seen: RSS 2.0 (S3 and most), Atom (MeteoAlarm, HK, DWD, NWS), a few custom indexes. Each entry links to one CAP 1.2 XML file; Content-Type is mostly `text/xml` or `application/cap+xml`.
- Cadence: not published per source. S3 mirrors are rebuilt when an alert arrives (feed `Last-Modified` moves); national servers regenerate at their own pace (MeteoAlarm Atom `updated` was within about 15 min). Poll each feed every 1 to 5 min with conditional GET when the host supports it (S3: yes; MeteoAlarm Atom: no ETag/Last-Modified, `Cache-Control: max-age=0, private, must-revalidate`, so no).
- Conditional requests, per host: S3 mirrors yes (ETag + Last-Modified); WMO-hosted `severeweather.wmo.int` sources.json yes (304); `dd.weather.gc.ca` files send ETag + Last-Modified; MeteoAlarm Atom no; most national servers UNVERIFIED (of 150 live feeds, 97 sent `Last-Modified`; ETag was only recorded for some hosts, so treat ETag support as unknown except for S3).
- Sampling method: fetched the feed of **all 156** sources once, then the first up to 3 CAP files of every feed that had items (277 CAP files).
  Results: 150 feeds answered 200; 6 failed from this machine (Antigua 500; Kuwait, Kyrgyzstan, Korea, St Vincent, Viet Nam timed out at 25 s: UNVERIFIED, may be transient or geo-slow); **46 feeds were empty today** (nothing to classify); 5 had items but the CAP fetch failed or did not parse (Burkina Faso, Colombia, Jamaica, Mali, South Africa: UNVERIFIED); **99 sources yielded at least one CAP**.
- Geometry among the 99 sources with a CAP (China only returned `Cancel` messages, which carry no area, so it is excluded from the percentage):

| Geometry in the CAP `<area>` | Sources | Share of 98 |
|---|---|---|
| `<polygon>` (79) or `<circle>` only (1, Niue) | 80 | **81.6 %** |
| `<geocode>` only (no polygon/circle) | 18 | 18.4 % |
| description only | 0 | 0 % |

  By region (polygon or circle / sampled): Africa 32/32, South America 7/7, SW Pacific 8/9, N and C America 10/11, Asia 10/11 (excl. China), Europe 13/28.
  Geocode-only sources: 14 European countries behind MeteoAlarm (EMMA_ID and NUTS3 codes: HR, CY, GR, IT, LV, LT, ME, PL, PT, RO, RS, SK, FR, BG), Australia BoM (`AMOC-AreaCode`), India NDMA (`LGD District Code`), Ireland (FIPS-like codes), US marine (UGC/SAME).
  Caveat: a source can mix both; each source was judged on up to 3 current alerts, and a quiet day biases the sample toward countries that happened to have alerts. 46 + 11 of 156 sources (37 %) are not covered by this measurement. Counting those unknown as "no usable geometry", the confirmed share is 80/156 = 51 %.
- Languages (primary language subtag, 99 sources): English 70, French 14, Spanish 10, Portuguese 5, Arabic 3, Chinese 2, Greek 2, Swedish 2, Norwegian 2, one each of Bulgarian, Rundi, Croatian, Estonian, Finnish, Indonesian. **29 of 99 are multi-language** (several `<info>` blocks: e.g. Canada en-CA + fr-CA, MeteoAlarm en-GB + national language, Macao en/pt/zh). Some sources omit `<language>` (NZ, Philippines, Singapore, Trinidad).
- msgType seen in 253 sampled CAPs: Alert 202, Update 46, Cancel 5. `<references>` was present in all Update/Cancel messages and in no plain Alert.
- `<references>` format (CAP 1.2 standard): space-separated list of `sender,identifier,sent`. Real examples:
  - Portugal: `https://www.ipma.pt,2.49.0.0.620.0.PT.261105122746.AZ-10-665,2026-11-05T12:27:46+00:00` (previous version only).
  - Norway: three entries (cumulative chain of all earlier versions). Australia BoM and Finland also list several earlier versions.
  - China: `Cancel` referencing the earlier alert.
  - Identifier shapes differ widely: OID style (`2.49.0.1.578.0.20261106162909.100`), URN, timestamps (`AusBoM-IDV29000-2026-11-06T17:32:39+00:00`), numeric (`53082641600000_20261107021316`). Sender is an e-mail, URL, URN or a plain name.
- Terms: WMO page links `https://wmo.int/copyright` and `https://wmo.int/disclaimer`; feeds carry `<copyright>public domain</copyright>` (S3 mirrors); each national service has its own licence (see per-source `metLink`). No key or account anywhere. UNVERIFIED: any explicit automated-access limit for the S3 mirrors (none stated).

### A2. MeteoAlarm (Europe)

- Per-country Atom: `https://feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-<country-slug>` (OK, e.g. `-bulgaria`, `-austria`; index of all countries at `https://feeds.meteoalarm.org/`, with "WebSub" subscribe links; the feed declares a PubSubHubbub hub).
  Slugs include `andorra`, `bosnia-herzegovina`, `czechia`, `republic-of-north-macedonia` (in the WMO list), `united-kingdom`. ~35 countries.
- Feed rights text: "Licensed under terms equivalent to CC BY 4.0, with additional requirements for redistributing outlined in our Terms and Conditions" (T&C not read: UNVERIFIED details).
- Entry shape: **one `<entry>` per (info, area, geocode)**, not per alert; entries share the same CAP link. Trimmed real example:

```xml
<entry>
  <cap:geocode><valueName>NUTS3</valueName><value>BG332</value></cap:geocode>
  <link title="Dobrich" href="https://meteoalarm.org?geocode=EMMA_ID:BG003" hreflang="en"/>
  <cap:areaDesc>Dobrich</cap:areaDesc>
  <cap:event>Strong wind!</cap:event>
  <cap:sent>2026-11-06T10:00:00+00:00</cap:sent> <cap:expires>2026-11-07T22:00:00+00:00</cap:expires>
  <cap:severity>Moderate</cap:severity> <cap:message_type>Alert</cap:message_type>
  <cap:identifier>2.49.0.0.100.0.BG.261106120000.061126114512Z1021</cap:identifier>
  <link type="application/cap+xml" href="https://feeds.meteoalarm.org/api/v1/warnings/feeds-bulgaria/56b586da-ab99-47f9-b23d-ff680c016ff6"/>
  <title>Yellow Wind Warning issued for Bulgaria - Dobrich</title>
</entry>
```
- CAP geometry: geocode only for most countries (EMMA_ID and/or NUTS3; sampled: BG, HR, CY, GR, IT, LV, LT, ME, PL, PT, RO, RS, SK, FR). Some countries do send polygons through their own feeds (DE DWD, FI, NO, SI, EE, IL, UA, ES AEMET).
- A key-free JSON dump per country also works: `https://feeds.meteoalarm.org/api/v1/warnings/feeds-<country>` (OK, 580 KB for Bulgaria; `{"warnings":[{"alert":{identifier,info:[{area:[{areaDesc,geocode:[{value,valueName}]}]}]}}]}`), so one request gives all alerts.
- EMMA_ID / NUTS3 to polygon: **no official public GeoJSON found**. `https://www.meteoalarm.org/webapps/meteoalarm/geocodes.geojson` returns an HTML page ("undefined"); `https://feeds.meteoalarm.org/api/v1/geocodes` is 404. A forum thread mentions a ~22 MB `geocodes.json` plus `geocode-aliases.csv` on the Open MeteoAlarm site (UNVERIFIED, not located). NUTS3 polygons are available from Eurostat GISCO (UNVERIFIED, not fetched). A GeoSphere Austria ArcGIS layer named `MeteoAlarm_Geocodes_Gaps` exists (UNVERIFIED).
- The newer API portal `https://api.meteoalarm.org/` (EDR-style, `/edr/v1/`) is a web page and per its portal needs registration/token (portal HTML only fetched; token requirement from page title "API Portal"; UNVERIFIED in detail). Not needed: the legacy Atom and `/api/v1/warnings/` JSON are open.

### A3. Canada (ECCC)

- Datamart CAP tree (OK): `https://dd.weather.gc.ca/today/alerts/cap/<YYYYMMDD>/<office>/<HH>/<file>.cap` (e.g. `.../20261106/CWWG/16/T_WWCN16_C_CWWG_202611061649_2992262779.cap`). The older `https://dd.weather.gc.ca/alerts/cap/` is 404; use `/today/`. Offices: CWHX, CWNT, CWUL, CWVR, CWWG. Directory indexes show Last modified, so new files are found by listing the current and previous hour directories; files send ETag + Last-Modified and never change once written (each update is a new file).
- Content-Type `application/cap+xml`. Polygons: **yes**, one `<polygon>` per `<area>` (lat,lon pairs), plus CLC and CAP-CP location geocodes. Languages: separate `<info>` for `en-CA` and `fr-CA`.
- Updates/cancels: `<msgType>` Alert/Update/Cancel with `<references>` as `sender,identifier,sent` (cumulative: an Update listed 4 earlier identifiers; plain Alert has `<references/>`). Sender `cap-pac@canada.ca`; identifier `urn:oid:2.49.0.1.124.<n>.2026`.
- Alternative (OK): OGC API `https://api.weather.gc.ca/collections/weather-alerts/items?f=json&limit=1` returns GeoJSON polygons with `publication_datetime`, `expiration_datetime`, alert_code/type, en/fr text. Collection list: `https://api.weather.gc.ca/collections?f=json`.
- The Pelmorex NAADS feed (`https://rss.naad-adna.pelmorex.com/`, in the WMO list, 3 CAPs OK) is a second source, plus Alberta `emergencyalert.alberta.ca/aeapublic/feed.atom` (OK).
- Licence: ECCC open data (Open Government Licence Canada: UNVERIFIED, not read this session). Each CAP also carries a `<note>` service notice.

### A4. Other regions not (or only partly) covered by the WMO hub

- Japan JMA: **not in the WMO list**. Atom index of JMAXML (not CAP): `https://www.data.jma.go.jp/developer/xml/feed/extra.xml` (OK, 122 KB, 208 entries, ETag + Last-Modified; warnings are `VPWW53/54` XML files by prefecture and area code, no polygons; the feed is Japanese-only). JSON alternative: `https://www.jma.go.jp/bosai/warning/data/warning/<prefcode>.json` (OK for 130000; area codes only). Area boundaries come from JMA GIS data (UNVERIFIED, not fetched). Terms: `https://www.jma.go.jp/jma/en/copyright.html`, free reuse with source credit (not read in full).
- Australia BoM: in the WMO list through `severeweather.wmo.int/v2/cap-alerts/au-bom-en/rss.xml` (OK, 349 KB); CAP has `AMOC-AreaCode` geocodes only (no polygon), languages en-AU, Updates reference several earlier versions. Polygons would need BoM's area boundary files (UNVERIFIED).
- New Zealand: in the list, `https://alerts.metservice.com/cap/rss` (polygons, no `<language>`).
- Mexico: `https://smn.conagua.gob.mx/tools/PHP/feedsmn/cap.php` (polygons, es-MX). Brazil INMET: `https://apiprevmet3.inmet.gov.br/avisos/rss` (polygons, pt-BR). Hong Kong: `https://alerts.weather.gov.hk/V1/cap_atom.xml` (200, empty today; geometry UNVERIFIED). South Africa SAWS: `https://caps.weathersa.co.za/Home/RssFeed` (672 items listed, CAP fetch failed: geometry UNVERIFIED). India: NDMA SACHET `https://sachet.ndma.gov.in/cap_public_website/rss/rss_india.xml` (geocode only: LGD district code) and IMD S3 mirror (empty today).
- Key-gated alternatives were not pursued (none needed).

### A5. De-duplication key for a generic CAP pipeline

What the real feeds show:
- `<identifier>` is unique per message and differs between an alert and its update, so it cannot be the hazard key. Formats vary (OID, URN, timestamp, number).
- `<references>` is present on every Update and Cancel in the samples, but the chain is inconsistent: some feeds list only the immediately previous message (PT, AR), others list all previous messages (NO, AU, CA, FI). Format `sender,identifier,sent` is consistent.
- VTEC does not exist outside the US.

Recommended: treat every CAP as a node keyed by `(sender, identifier)`. On ingest, for each `(sender, identifier)` in `<references>`, union it with the new node (union-find, or a `thread_id` column copied from the oldest referenced row). The thread root is the hazard key: `thread_id = hash(sender, root identifier)`. Rules:
- `Alert`: new thread (unless an unseen reference points to something we already have).
- `Update`: same thread; replace geometry, text and expiry; notify only if the new area newly covers a saved place or severity rose.
- `Cancel`: same thread; close it now.
- Also close any thread at `<expires>` (S3 mirrors keep stale items). Ignore `status != Actual` (Test, Exercise) and `msgType = Ack/Error`.
- Fallback when references are missing or broken: `(sender, event, onset, area signature)`, where area signature is a hash of the polygon or geocode set rounded to 2 decimals. Use only as a safety net since feeds re-issue with changed onset.
- MeteoAlarm: dedupe on the CAP link/identifier, not the Atom entry id (entries repeat per area).

## B. Point forecast anywhere

### B1. MET Norway Locationforecast 2.0 (recommended)

- Endpoint (OK): `https://api.met.no/weatherapi/locationforecast/2.0/compact?lat=-1.2921&lon=36.8219` (Nairobi). Also `/complete`, `/classic` (legacy XML), `&altitude=<m>` optional (ground-surface altitude; otherwise a coarse model topography is used).
- Global: yes ("for any location on earth", up to nine days). Nairobi sample returned elevation 1660 m.
- Response (GeoJSON Feature, 39 KB for compact): `properties.meta.units` (air_pressure_at_sea_level hPa, air_temperature celsius, cloud_area_fraction %, precipitation_amount mm, relative_humidity %, wind_from_direction degrees, wind_speed m/s), then `timeseries[]`. In the sample: **88 entries, 60 hourly then 27 steps of 6 hours**, from 2026-11-06T18:00Z to 2026-11-16T00:00Z. Each entry has `instant.details` plus optional `next_1_hours`, `next_6_hours`, `next_12_hours` (each with `summary.symbol_code` and `details.precipitation_amount`; 1 h blocks only in the hourly part, 6 h blocks throughout, 12 h blocks mostly in the first days). `apparent_air_temperature` was added 2026-08-31 (documentation changelog; not in the compact sample).
- Symbol codes (strings like `clearsky_day`, `partlycloudy_night`, `lightrainshowers_day`, `rain`, `cloudy`); seen: clearsky_night, cloudy, lightrain, lightrainshowers_day/night, partlycloudy_day/night, rain, rainshowers_day. Day/night variants end `_day`, `_night`, `_polartwilight`. Icons are MIT licensed, listed at the docs "Weather icons" page (`https://api.met.no/weatherapi/weathericon/2.0/documentation`: UNVERIFIED, not fetched).
- Caching headers (OK): `Expires: Fri, 06 Nov 2026 18:44:28 GMT`, `Last-Modified`. `If-Modified-Since` with the exact previous `Last-Modified` returned 304 (tested). Per the docs: data refreshes hourly for the Nordic region and every 6 h elsewhere; do not repeat an identical request more often than every 30 min (Nordic) or **every 3rd hour (rest of the world)**; never before `Expires`.
- Terms of Service (read 2026-11-06, last revised 2026-11-05):
  - Traffic: under 20 requests/second total per application; do not repeat identical requests before `Expires`; use `If-Modified-Since` (the value must equal the last `Last-Modified`, not a made-up time); do not do HEAD then GET; spread scheduled requests with a random minute offset; gzip and redirects must be supported; HTTPS only.
  - Coordinates: truncate to **at most 4 decimals**. The ToS says 5+ decimals returns 403 for new products. Test today: lat/lon with 6 decimals returned 200 and the response snapped to 4 decimals. Do not rely on that; truncate.
  - Mobile or browser clients should go through a caching proxy (backend); apps must not poll while not in use; push notification polling no more than once per 10 min (for MetAlerts).
  - Throttling gives 429; 203 status means a deprecated product (log it).
  - **User-Agent**: must identify the application/domain (+ optional version) AND provide contact information. FAQ says valid contact is an e-mail (preferably non-personal), a website with technical contact details, or a link to a GitHub repo where issues can be opened (forums and private repos not accepted). Generic strings like `MyTestApp` or `wget` cause 403/429, and fake or random identification can lead to a permanent ban. Test result today: **`User-Agent: weather-map-dev` returned 200** (three requests). That is a luck-of-the-filter result and does not meet the stated rule (no contact info); the production server must send something like `<app-name>/<version> <public GitHub repo URL or project site>`. Not tested with any contact string, to avoid sending personal details.
  - Licence: data under NLOD 2.0 and CC BY 4.0; credit "Data from MET Norway" / "Based on data from MET Norway" (a link to the source is appreciated); say if changed; do not imply endorsement; commercial use allowed. Do not name the service "Yr" or use the Yr logo. Show users the retrieval time.
  - AI note in ToS: the terms must be read by a human before use.
- Requests made to api.met.no this session: 4 (3 forecasts, 1 conditional) plus ToS/docs pages.

### B2. Other key-free global point forecasts

- Open-Meteo `https://api.open-meteo.com/v1/forecast?latitude=-1.29&longitude=36.82&hourly=temperature_2m&forecast_days=1` (OK, no key). Terms page (`https://open-meteo.com/en/terms`, read): free API is **for non-commercial use only**, under 10,000 calls/day, 5,000/hour, 600/minute; you accept CC BY 4.0 (attribution); they may block misuse; commercial use requires a paid plan (`https://open-meteo.com/en/pricing`, not read in detail). Because the project's own status (portfolio vs. commercial) is the deciding factor, MET Norway is the safer default; Open-Meteo is a fallback for personal use. The server snaps coordinates (-1.29 became -1.3005).
- NWS api.weather.gov is US only (not touched). Others (e.g. Met Office DataPoint, Tomorrow.io, OpenWeather) need keys: sign-ups `https://www.metoffice.gov.uk/services/data/datapoint`, `https://home.openweathermap.org/users/sign_up`.

## C. Surface observations

### C1. aviationweather.gov METAR cache (global, OK)

- `https://aviationweather.gov/data/cache/metars.cache.csv.gz` (287 KB gz). Headers: `Last-Modified`, `ETag`, `Cache-Control: max-age=30`; `If-Modified-Since` returns 304. The file refreshes about once a minute.
- Header line: `raw_text,station_id,observation_time,latitude,longitude,temp_c,dewpoint_c,wind_dir_degrees,wind_speed_kt,wind_gust_kt,visibility_statute_mi,altim_in_hg,sea_level_pressure_mb,...,metar_type,elevation_m`. Example row: `"METAR LWOH 061820Z 30003KT 270V040 CAVOK 04/03 Q1021",LWOH,2026-11-06T18:20:00.000Z,41.1810,20.7440,4,3,300,3,,6+,30.15,...,VFR,...,METAR,700`.
- **5,266 stations, all unique, global** (so the current US bounding-box filter drops about half). By ICAO first letter: K 2415 (US), C 492 (Canada), E 376 and L 423 (Europe, 799), S 346 (S America), P 180 (Pacific US), U 136 (Russia/CIS), M 136 (Mexico/C America), O 111 (Middle East/Pakistan), D 65 + G 62 + F 78 + H 38 (Africa, 243), V 86 (S/SE Asia), R 82 (E Asia), W 56 (Indonesia/Malaysia), Y 56 (Australia), Z 41 (China), N 41 (S Pacific/NZ), T 31 (Caribbean), B 15 (Iceland/Greenland). US share is about 50 %.
- Data quality: 24 rows have lat/lon `-99.99` (drop), 37 have no temperature; observation times in the file span 16:51 to 18:20Z (2,842 in the current hour, 2,410 in the previous hour), so stale reports exist and need an age filter.
- Terms: none stated on the cache page fetched; US-government product (AWC). UNVERIFIED: explicit usage policy. `Cache-Control: max-age=30`, so polling every 60 s is the cap; every 5 min is plenty.

### C2. NDBC latest observations (OK)

- `https://www.ndbc.noaa.gov/data/latest_obs/latest_obs.txt` (856 lines, 854 stations). Fixed-width text, two header lines: `#STN LAT LON YYYY MM DD hh mm WDIR WSPD GST WVHT DPD APD MWD PRES PTDY ATMP WTMP DEWP VIS TIDE`, units line, then rows with `MM` for missing. Example: `22101 37.24 126.02 2026 11 06 17 00 350 9.0 MM 0.5 0 MM MM MM MM 9.2 18.6 MM MM MM`. Longitude is signed (E positive). Wind in m/s, pressure hPa, temperature degC.
- Headers: ETag + Last-Modified, `Cache-Control: max-age=600` (CloudFront); updated about every 10 min.
- Coverage: 816 of 854 stations are in the western hemisphere north of the equator (US and Canada coast, Great Lakes, Gulf, Caribbean, Pacific buoys 46xxx), 34 in the eastern hemisphere (including Korean 22xxx buoys, a few WMO 5-digit partner buoys), 4 southern. It is mostly an Americas product, so it adds ocean points near the US and not much elsewhere. Ship reports: `ship_obs.txt` does not exist (404); ships UNVERIFIED. Directory index: `https://www.ndbc.noaa.gov/data/latest_obs/` (per-station files, not needed).
- Terms: NOAA public domain; UNVERIFIED explicit statement.

### C3. Other single-file, key-free, many-station feeds

| Source | URL (OK today) | Stations | Cadence | Coordinates | Notes |
|---|---|---|---|---|---|
| Japan JMA AMeDAS | data: `https://www.jma.go.jp/bosai/amedas/data/map/20261107031000.json` (latest time from `.../data/latest_time.txt`, here `2026-11-07T03:10:00+09:00`); stations: `https://www.jma.go.jp/bosai/amedas/const/amedastable.json` | 1,286 in both files | 10 min | in table (`lat: [deg, min]`, `lon`, `enName`) | map file 250 KB: per station `temp, humidity, wind, windDirection, precipitation*` as `[value, qualityflag]`; wind in m/s. Terms: JMA copyright page (credit JMA). |
| Sweden SMHI | `https://opendata-download-metobs.smhi.se/api/version/1.0/parameter/1/station-set/all/period/latest-hour/data.json` | 231 (parameter 1 = air temperature) | hourly; `max-age=600` | in each station object | parameter ids differ for wind (4 direction, 3 speed? UNVERIFIED mapping); one request per parameter. Licence CC BY 4.0 (UNVERIFIED, not read). |
| Switzerland MeteoSwiss | `https://data.geo.admin.ch/ch.meteoschweiz.messwerte-aktuell/VQHA80.csv` | about 160 | 10 min | NOT in the file (station codes only like TAE, COM; metadata is a separate file, UNVERIFIED location) | semicolon CSV with `tre200s0` (temp), `fu3010z0` (wind), etc. Open government data. |
| Austria GeoSphere TAWES | metadata `https://dataset.api.hub.geosphere.at/v1/station/current/tawes-v1-10min/metadata`; data endpoint `.../tawes-v1-10min?parameters=TL,FF&station_ids=...` | many hundreds | 10 min | in metadata | data call not tested (UNVERIFIED); CC BY 4.0 presumed. |
| Finland FMI | `https://opendata.fmi.fi/wfs?service=WFS&version=2.0.0&request=getFeature&storedquery_id=fmi::observations::weather::simple&bbox=24,60,25,61&parameters=t2m&starttime=...&endtime=...` | 22 returned for a 1 degree box | 10 min | in GML | open WFS, XML/GML (14 KB); CC BY 4.0 (UNVERIFIED). |
| Canada ECCC SWOB | `https://api.weather.gc.ca/collections/swob-realtime/items?f=json&limit=1` | one item per observation (13.9 million total), not a station snapshot | 1 to 60 min | in geometry | needs `datetime=` and `bbox=` filters to build a snapshot (not tested); datamart alternative is per-station files. |
| Germany DWD | `https://opendata.dwd.de/weather/weather_reports/poi/` | one file per station (`01008-BEOB.csv`) | hourly | no | per-station crawl: skip. |

Failed or not working today: INMET Brazil `https://apitempo.inmet.gov.br/estacao/dados/2026-11-06/1200` returned 204 empty (UNVERIFIED; the endpoint is meant to list all automatic stations per hour); SMN Mexico `https://smn.conagua.gob.mx/webservices/?method=1` and IMGW Poland `https://danepubliczne.imgw.pl/api/data/synop` timed out at 30 to 40 s (UNVERIFIED).

Key-gated (record only): MET Norway Frost (client id) `https://frost.met.no/auth/requestCredentials.html` (UNVERIFIED URL); NOAA MADIS `https://madis.ncep.noaa.gov/` (account); KNMI Open Data API (key) `https://developer.dataplatform.knmi.nl/`; DMI Open Data (key) `https://opendatadocs.dmi.web.dmi.dk/`; Meteo-France `https://portail-api.meteofrance.fr/`; UK Met Office DataHub `https://datahub.metoffice.gov.uk/`; Korea KMA `https://data.kma.go.kr/` (all UNVERIFIED sign-up pages, listed from memory).

## Summary of recommendations

1. Alerts: use `severeweather.wmo.int/json/sources.json` as the registry (re-read daily with If-Modified-Since), poll each `capAlertFeed`, use the dedupe thread key above, and add direct ECCC Datamart for Canada.
2. Polygons exist for about 82 % of the sources that had live alerts (51 % of all 156 confirmed); the remaining geocode-only sources (mainly MeteoAlarm countries) need a geocode-to-polygon table (EMMA_ID or NUTS3) that must be built from Eurostat or the MeteoAlarm geocode files (not located).
3. Point forecast: MET Norway Locationforecast 2.0 with a proper contact User-Agent, 4-decimal truncation, server-side cache honoring `Expires` and a 3-hour minimum for non-Nordic points.
4. Stations: drop the US bounding box on the METAR CSV (5,266 stations), keep NDBC for buoys, add JMA AMeDAS and SMHI as optional density feeds.
