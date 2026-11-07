# Canada and rest-of-world sources (verified 2026-11-06, ~18:15-18:35 UTC)

All facts below were fetched on 2026-11-06 unless marked UNVERIFIED. Samples are in
`/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/canada-world/`
(not in the repo). Decoding was done with eccodes 2.50 (JPEG2000 included), numpy, netCDF4, omfiles.
Europe and the US are covered elsewhere. Requests used `User-Agent: weather-map-dev`, <= 2 req/s.

## 0. Summary table

| Source | Usable (no key) | Coverage | Resolution | Cadence / latency | Format | Grid tmpl | Packing tmpl | Key gotcha |
|---|---|---|---|---|---|---|---|---|
| ECCC GDPS 15 km | yes | global | 0.15 deg (2400x1201) | 00/12Z, hourly to 240 h; f000 at +4h01, f240 at +5h18 | GRIB2, one var per file | 3.0 | **5.40 JPEG2000** | need JPEG2000 decoder; only 2 runs/day |
| ECCC RDPS 10 km | yes | N. hemisphere wide (lat -3.8..90, all lon) | 0.09 deg rotated | 00/06/12/18Z presumed (00/06/12 seen), 84 h hourly; +2h40 f000, +3h04 f084 | GRIB2 | **3.1 rotated lat-lon** | 5.40 | rotated pole, grid-relative winds |
| ECCC HRDPS 2.5 km | yes | Canada + N. US, lat 27.3..70.6, lon -152.7..-40.7 | 0.0225 deg rotated (2540x1290) | 00/06/12/18Z, 48 h hourly; +2h48 f000, +3h17 f048 | GRIB2 | 3.1 | 5.40 | big (about 1.3 GB per run for our 12 vars); no reflectivity |
| ECCC radar 1 km NA composite | partly: coloured only | lat 16.9..67.2, lon -170.3..-50 | 1 km | 6 min, ~8 min late | PNG from WMS (no raw) | n/a | n/a | invert 14-colour legend; no WCS for radar |
| ECCC CAP alerts | yes | Canada | polygons | event-driven | CAP 1.2 XML | n/a | n/a | poll 5 offices x hour dirs |
| ECCC SWOB | yes | Canada ~1270 stn | point | hourly + minute files | XML per station | n/a | n/a | no nationwide file |
| JMA nowcast radar tiles | yes | Japan | ~1 km, XYZ z<=10ish | 5 min, ~0-2 min late; obs 3 h back, +60 min forecast | palette PNG tiles | n/a | n/a | 8-colour palette to rain-rate bins |
| JMA GSM/MSM raw GRIB | no (gated) | | | | | | | JMBSC; see section 9 |
| Open-Meteo bucket: JMA MSM | yes | lat 22.4..47.6, lon 120..150 | 0.05 x 0.0625 deg (505x481) | 3-hourly runs, 79 valid times hourly; +5h | .om (OM v3) | regular lat-lon | PFor delta2d int16 | custom format, no gust/ptype |
| Open-Meteo bucket: CMA GRAPES global | yes | global | 0.125 deg | 06Z run done +6h28 | .om | regular | same | has gusts, ptype, snowfall; upstream licence UNVERIFIED |
| Taiwan CWA 3 km WRF (M-A0064) | yes | lat 14..32, lon 105..141 | 3 km | 12Z (00Z presumably), 6-hourly files to 84 h; +5-6 h | GRIB2 | 3.30 Lambert | 5.0 (24 bit) | 6-hourly only; no gust/cloud/ptype |
| Taiwan CWA global (M-A0060) | yes | global | 0.25 deg | 6-hourly to 384 h, 62 MB per step | GRIB2 | 3.0 (UNVERIFIED, doc says N->S) | UNVERIFIED | huge files |
| Taiwan CWA radar (O-A0059-001) | yes | lat 18..~29, lon 115..~126.5 | 0.0125 deg (921x881) | 10 min, ~7 min late | JSON text (comma floats), dBZ | n/a | n/a | 8.9 MB text per frame |
| Argentina SMN WRF 4 km | yes | lat -56.9..-11.6, lon -94.3..-35.7 | 4 km Lambert (999x1249) | 00/06/12Z(/18?), 72 h hourly; 00Z/06Z complete +2h40; 12Z still partial at +6h | NetCDF4/HDF5 | LCC (CF) | zlib+shuffle | speed+direction not u/v; no gust/cloud/ptype |
| Brazil CPTEC WRF 7 km | yes | lat -57.9..17.7, lon -90.7..-19.4 | 0.07 deg (1019x1081) | 00Z only, hourly to ~180 h; +5h20 | GRIB2 one file per valid time (204 MB, .inv index) | 3.0 | **5.3** | has GUST and REFC; no ptype/snow |

## 1. ECCC MSC Datamart: paths and naming

- Hosts: `https://dd.weather.gc.ca/` and mirror `https://hpfx.collab.science.gc.ca/` (same layout; hpfx root lists dated dirs ~50 days back).
- Current layout is **`/YYYYMMDD/WXO-DD/<product>/...`** (verified 200: `https://dd.weather.gc.ca/20261106/WXO-DD/model_hrdps/continental/2.5km/12/003/...`). `/today/<product>/...` is an alias for the current UTC date (directory listing works; no `WXO-DD` segment under `/today/`). Dated dirs also exist for the past ~30 days on dd.
- Directory listings (Apache) are in UTC; `Last-Modified` headers are GMT. Use them for latency.
- Docs: https://eccc-msc.github.io/open-data/ . File naming: `{YYYYMMDD}T{HH}Z_MSC_{MODEL}_{VAR}_{LVLTYPE-LVL}_{grid}_PT{hhh}H.grib2`, one variable and one forecast hour per file, in `/{HH}/{hhh}/`.
- Licence: ECCC Data Services End-use Licence v2.1.1 (commercial use allowed, copy/modify/distribute). Required attribution: "Data Source: Environment and Climate Change Canada" or, if no specific statement is practical, "Contains information licenced under the Data Server End-use Licence of Environment and Climate Change Canada." Provide a link to the licence where possible (https://eccc-msc.github.io/open-data/licence/readme_en/). No rate limit text found on the usage page; keep politeness.
- Finding newest complete run: list `/{YYYYMMDD}/WXO-DD/model_x/.../` for HH dirs, then check that the last forecast-hour dir (HRDPS 048, RDPS 084, GDPS 240) contains your variable file (HEAD 200). Files appear in forecast-hour order, so the last-hour file is the completeness signal.

### 1.1 HRDPS (continental 2.5 km)
- Path: `/YYYYMMDD/WXO-DD/model_hrdps/continental/2.5km/{HH}/{hhh}/`; runs 00/06/12/18Z; hhh 000..048 hourly.
- Latency (Last-Modified of TMP_AGL-2m): 12Z f000 14:48, f001 14:58, f024 15:05, f048 15:17 (so run complete ~3h17). 06Z: f000 08:48, f048 09:14. 00Z f000 02:57 but f001 only 04:39 and f048 06:42 (a slow run; allow ~3.5-4.5 h).
- Concrete 200 URL: `https://dd.weather.gc.ca/20261106/WXO-DD/model_hrdps/continental/2.5km/12/003/20261106T12Z_MSC_HRDPS_TMP_AGL-2m_RLatLon0.0225_PT003H.grib2` (3.28 MB).
- File sizes (f003): TMP 3.3 MB, DPT 1.6, RH 2.5, UGRD 1.9, VGRD 2.0, GUST 2.4, APCP 2.5, PRATE 0.25, PTYPE 1.4, TCDC 2.9, WEASN 1.4, SDWE 0.29.
- Variable tokens (all `20261106T12Z_MSC_HRDPS_<token>_RLatLon0.0225_PT003H.grib2`):

| Need | Token | GRIB2 (disc/cat/num) | Units / semantics |
|---|---|---|---|
| T 2 m | `TMP_AGL-2m` | 0/0/0 | K, instant |
| Td 2 m | `DPT_AGL-2m` (also `DEPR_AGL-2m`, `RH_AGL-2m`) | 0/0/6; RH 0/1/1 | K; RH in % |
| 10 m u / v | `UGRD_AGL-10m`, `VGRD_AGL-10m` (also `WIND_AGL-10m`, `WDIR_AGL-10m`) | 0/2/2, 0/2/3 | m/s, **grid-relative** (see below) |
| 10 m gust | `GUST_AGL-10m` (also `GUST-Max_`, `GUST-Min_` AGL-10m) | 0/2/22 | m/s, instant |
| Precip accum | `APCP_Sfc` = accumulated since run start (stepRange 0-3, PDT 4.8, stat proc 1); `APCP-Accum1h_Sfc` (2-3), `APCP-Accum3h_Sfc` | 0/1/8 | kg/m2 (= mm) |
| Precip rate | `PRATE_Sfc` | 0/1/7 | kg m-2 s-1, instant |
| Precip type | `PTYPE_Sfc` | 0/1/19 | code table 4.201 flagged by eccodes; values 1..9 observed at f003. Meaning of each code UNVERIFIED against ECCC doc (standard 4.201: 1 rain, 3 freezing rain, 5 snow, 6 wet snow, 7 rain/snow mix, 8 ice pellets). Alternative: separate liquid-equivalent amounts `WEARN_Sfc`, `WEASN_Sfc`, `WEAFR_Sfc`, `WEAPE_Sfc` (rain, snow, freezing rain, ice pellets; each with `-Accum1h`, `-Accum3h`) and a post-processed `HRDPS-WEonG_*` family (e.g. `HRDPS-WEonG_PCPNTYPE_Sfc`, `WEASN`, `APCP`, `WEARN`). |
| Total cloud | `TCDC_Sfc` | 0/6/1 | % |
| Snowfall accumulation | `WEASN_Sfc` (since run start, 0-3) and `WEASN-Accum1h_Sfc`: liquid-equivalent snow, kg/m2 (PDS: `tsnowp`). Depth needs a ratio (~10:1) or use `SNOD_Sfc`/`SDWE_Sfc` (snow water equiv. on ground, kg/m2, not an accumulation). | 0/1/66 | kg/m2 |
| Reflectivity | none found in the f000/f003/f048 surface listings (no REF/DBZ token). UNVERIFIED for other hours. | | |

- Grid (identical for all HRDPS files): **GRIB2 grid template 3.1 (rotated lat-lon)**, shape of earth 6 (sphere R=6371229), Ni=2540, Nj=1290, first point lat -12.302501, lon 345.17878; last point lat 16.700001, lon 42.306283 (these are rotated-frame coordinates), di=dj=0.0225 deg, scan +i, +j (jScansPositively=1, row-major), southern pole of rotation lat -36.08852, lon 245.305142, angle of rotation 0. Resolution/component flags = 56: bit 5 set so **u/v are relative to the (rotated) grid**, you must rotate the vector to true north/east. True lat/lon bbox of grid points: lat 27.28..70.61, lon -152.73..-40.71 (corners: SW (39.63,-133.63), SE (27.28,-66.97), NW (66.57,-152.73), NE (47.88,-40.71)).
- Data representation: **5.40 (JPEG2000)** in every file tested (13 files), 12-24 bits per value, no bitmap (bitMapIndicator 255), missing value 9999 declared but not used in the files sampled.
- Samples: `.../hrdps/*.grib2` (14 files, 25 MB).
- No "north" domain exists at `/today/model_hrdps/north/` (empty listing). The 1 km nested system is in `model_hrdlps/` (listing only checked at top level, not fetched).
- Also present at top level: `model_hrdpa` (2.5 km gauge-adjusted precipitation analysis, 6h/24h accumulations, 00/06/12Z, RLatLon0.0225, listed ~1h after nominal; not examined further).

### 1.2 RDPS (10 km)
- Path `/YYYYMMDD/WXO-DD/model_rdps/10km/{HH}/{hhh}/`; runs seen 00, 06, 12 (18 presumably, not yet issued today); hhh 000..084 hourly.
- **New long variable names** (not the CMC short ones): `20261106T12Z_MSC_RDPS_<Var>_<Lvl>_RLatLon0.09_PT003H.grib2`. 200 URL: `https://dd.weather.gc.ca/20261106/WXO-DD/model_rdps/10km/12/003/20261106T12Z_MSC_RDPS_AirTemp_AGL-2m_RLatLon0.09_PT003H.grib2`.
- Tokens: `AirTemp_AGL-2m`, `DewPoint_AGL-2m` (+`DewPointDepression_AGL-2m`, `RelativeHumidity_AGL-2m`), `WindU_AGL-10m`, `WindV_AGL-10m` (+`WindSpeed_`, `WindDir_`), `WindGust_AGL-10m` (+`WindGust-Max_/-Min_`), `Precip-Accum_Sfc` (since run start, 0-3; also `Precip-Accum1h_Sfc`, `Precip-Accum3h_Sfc`), `PrecipRate_Sfc`, `PrecipType-Instant_Sfc` (values 1..6 at f003), `Rain-Accum*_Sfc`, `Snow-Accum_Sfc` (+1h/3h; kg/m2 liquid equivalent, `tsnowp` 0/1/66), `FreezingRain-Accum*`, `IcePellets-Accum*`, `TotalCloudCover_Sfc`. No reflectivity token.
- Latency: 12Z f000 14:40, f084 15:04 (+3h04 complete).
- Grid: template 3.1, 1140 x 1045, first (-48.805954, 306.141412), last (45.464939, 48.990594), di=dj=0.090298, jScansPositively=1, south pole (-31.758312, 267.597031), rotation 0, flags 56 (grid-relative winds). True lat span -3.77..89.96; the domain covers the whole northern polar cap, i.e. most of the northern hemisphere. Corners: (-3.77,-124.61), (-1.64,-62.59), (46.63,143.16), (50.04,32.11).
- Packing 5.40. Sample: `rdps/` (10 files).

### 1.3 GDPS (global)
- Path `/YYYYMMDD/WXO-DD/model_gdps/15km/{HH}/{hhh}/`, runs **00 and 12 only**, hhh 000..240, **hourly** (all 241 dirs exist at +5h). A `model_gdps/10km/` dir exists with 00/06/18 listings but no files found under `10km/12/003/`; its contents were not explored (UNVERIFIED, possibly a stat-post-processing or pilot product).
- Names `..._MSC_GDPS_<Var>_<Lvl>_LatLon0.15_PT003H.grib2`, same variable tokens as RDPS (`AirTemp_AGL-2m`, `DewPoint_AGL-2m`, `RelativeHumidity_AGL-2m`, `WindU_AGL-10m`, `WindV_AGL-10m`, `WindGust_AGL-10m`, `Precip-Accum_Sfc`, `PrecipRate_Sfc`, `PrecipType-Instant_Sfc` (1..6), `Snow-Accum_Sfc`, `TotalCloudCover_Sfc`, `Rain-Accum*`, `FreezingRain-Accum*`, `IcePellets-Accum*`). No reflectivity.
- 200 URL: `https://dd.weather.gc.ca/20261106/WXO-DD/model_gdps/15km/12/003/20261106T12Z_MSC_GDPS_AirTemp_AGL-2m_LatLon0.15_PT003H.grib2` (about 1.3 MB per field).
- Latency: 12Z f000 16:01 (+4h01), f120 17:04, f240 17:18 (+5h18).
- Grid: **template 3.0 regular lat-lon**, Ni=2400, Nj=1201, lat -90..90 (first point -90, jScansPositively=1 i.e. south to north), lon 0..359.85, di=dj=0.15; shape 6; flags 48. Packing **5.40**. Samples: `gdps/`.
- Open-Meteo's mirror lists 137 valid times for GDPS 15 km (1-hourly then coarser to 240 h) and finishes later (+5h51), so prefer Datamart.

## 2. ECCC radar
- **Datamart (and hpfx)**: `/YYYYMMDD/WXO-DD/radar/` has only `24_HR_ACCUM/`, `CAPPI/`, `DPQPE/`, `visibility/`, each containing only `GIF/<site>/` per-site coloured GIFs (e.g. `radar/CAPPI/GIF/ATL/202611060000_ATL_CAPPI_1.5_RAIN_WT.gif`, 6-min). No GeoTIFF/HDF5/raw. Not usable as raw values.
- **GeoMet WMS** (`https://geo.weather.gc.ca/geomet`) layer `RADAR_1KM_RRAI` = "Radar precipitation rate for rain [mm/h]", North American composite, 1 km, up to 180 radars (Canadian + NEXRAD), bbox lon -170.32..-50.00, lat 16.93..67.19. Time dimension `2026-11-06T15:12:00Z/2026-11-06T18:12:00Z/PT6M` (3 h window, 6-min step); at 18:20Z the newest was 18:12Z (~8 min latency). Snow layer `RADAR_1KM_RSNO` (cm/h) presumably exists (UNVERIFIED by name; WMS capabilities list rain and snow concepts).
- **No raw values**: WCS (v2.0.1/1.1.1/1.0.0) GetCapabilities works for model coverages (e.g. `GDPS_15km_AirTemp`) but radar is not listed, and GetCoverage for `RADAR_1KM_RRAI` fails; WMS GetMap refuses image/tiff, geotiff, netcdf (returns XML exception); only image/png, jpeg, webp. `GetFeatureInfo` (text/plain) returns per-pixel `value_0` and `class` for a point.
- Colour legend source: `GetLegendGraphic` (`layer=RADAR_1KM_RRAI&format=image/png&STYLE=Radar-Rain_14colors`), saved as `radar/legend_rrai_14colors.png`. Legend labels (mm/h): 0.1, 1.0, 2.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0, 50.0, 64.0, 100.0, 125.0, 200.0 (14 classes). Other styles: `Radar-Rain_Dis-14colors`, `_Fr`, `Radar-Rain_8colors`. Quantisation to 14 bins is the best precision available; recommend `image/png` with `TRANSPARENT=true`, EPSG:4326 or 3857 bbox, and invert by nearest palette colour. Sample: `radar/getmap_rrai_sample.png` (900x450 over lat 40-55, lon -100..-70).
- Overlaps MRMS over the US; US radar sites inside this composite are NEXRAD (same ultimate source). Use the composite only north of the US border.
- Terms: same ECCC licence; WMS is a heavier on-demand service, so cache tiles/frames server-side, one fetch per 6 min.

## 3. ECCC alerts (CAP)
- Path: `/YYYYMMDD/WXO-DD/alerts/cap/YYYYMMDD/{office}/{HH}/T_WWCN16_C_{office}_{YYYYMMDDHHMM}_{id}.cap` with offices CWHX, CWNT, CWUL, CWVR, CWWG (as listed today); HH hour dirs exist only when something was issued. Example (200): `https://dd.weather.gc.ca/20261106/WXO-DD/alerts/cap/20261106/CWWG/16/T_WWCN16_C_CWWG_202611061649_2992262779.cap` (64 KB).
- Cheap discovery: each cycle list the current and previous HH directory of each of the 5 offices (10 small listings), diff against seen filenames. (AMQP push is also offered by MSC; not tested.)
- Content: CAP 1.2 (`urn:oasis:names:tc:emergency:cap:1.2`), profile CAP-CP 0.4, one file per alert message, contains `<polygon>` elements ("lat,lon lat,lon ..." space separated; 52 polygons in the sample) and `<geocode>`; `msgType` Alert/Update/Cancel with `<references>` for supersession. Bilingual (separate `<info>` blocks, en-CA / fr-CA). Sample: `alerts/sample.cap`.
- There is **no alerts collection** in the OGC API (`https://api.weather.gc.ca/collections`: hydrometric, climate, swob, aqhi, hurricanes, prognos, bulletins, citypageweather, marineweather, ...).

## 4. ECCC SWOB observations
- Datamart: `/YYYYMMDD/WXO-DD/observations/swob-ml/{YYYYMMDD}/{ICAO-like id}/{YYYY-MM-DD-HHMM}-{ID}-{AUTO|MAN}-swob.xml` (hourly; 848 files in today's dir) and **`observations/swob-ml/latest/`**: one rolling file per station (`CAAW-AUTO-swob.xml`, `CABF-AUTO-minute-swob.xml`, about 1272 entries). No single nationwide file. Approach: one listing request (mtime per file) then GET only changed stations (~1000 files/hour). Also `observations/swob-ml/partners/` and `marine/`.
- XML: `<element name="air_temp" uom="°C" value="14.4">` etc.; sample `swob/CAAW-AUTO-swob.xml` (8.8 KB).
- OGC API collection `swob-realtime` (https://api.weather.gc.ca/collections/swob-realtime/items?f=json) works (13.9 M features, 214 properties each incl. `air_temp`, `avg_wnd_spd_10m_pst1mt`) but defaults to oldest-first, `sortby=-date_tm` is rejected, and a `datetime=` range query timed out (>120 s) so it is not recommended for live use.

## 5. Japan (JMA)

### 5.1 Nowcast radar tiles (usable)
- Time lists (JSON arrays, newest first, UTC `YYYYMMDDhhmmss`):
  - `https://www.jma.go.jp/bosai/jmatile/data/nowc/targetTimes_N1.json`: observed, 37 entries every 5 min (3 h), `{"basetime","validtime","elements":["hrpns","hrpns_nd"]}`; newest 18:20:00 at ~18:22 UTC.
  - `targetTimes_N2.json`: forecast, `basetime` fixed (18:20), `validtime` +5 min steps to +60 min.
  - `targetTimes_N3.json`: thunder/tornado products (`thns`, `trns`) at 10-min steps.
- Tile URL: `https://www.jma.go.jp/bosai/jmatile/data/nowc/{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png` (XYZ Web Mercator, 256 px). Verified 200: `.../nowc/20261106182000/none/20261106182000/surf/hrpns/6/56/25.png` (1484 B). z3..z11 all returned 200 (empty tiles are 334 B transparent PNGs); real data resolution ends around z10 (UNVERIFIED). `cache-control: max-age=86400`, `Last-Modified` equals the valid time (18:20:21Z).
- Format: 8-bit palette PNG with tRNS transparency. Palette seen: transparent (no echo), (242,242,255) #f2f2ff, (160,210,255) #a0d2ff, (33,140,255) #218cff, (0,65,255) #0041ff, (250,245,0) #faf500, (255,153,0) #ff9900, (255,40,0) #ff2800 (plus a purple #b40068 for the top class, not seen in samples). JMA's published legend bins are 1-5, 5-10, 10-20, 20-30, 30-50, 50-80, 80+ mm/h (from memory, UNVERIFIED against the official legend page). `hrpns_nd` is presumably the no-data-masked variant (UNVERIFIED). Samples: `jma/hrpns_6_56_25.png`, `jma/hrpns_8_226_100.png`.
- Terms: JMA website terms (https://www.jma.go.jp/jma/en/copyright.html): Public Data License v1.0 (CC BY compatible). Cite "Source: Japan Meteorological Agency website (URL of relevant page)" and state that content was edited if processed; do not present edited data so that it could be mistaken for official government output. No explicit automated-access clause found; these are the same tiles the JMA site uses, be gentle.

### 5.2 JMA GSM/MSM GRIB
- Not obtainable anonymously (JMA GPV data are distributed via JMBSC registration; older HR-GSM service needed an ID; Kyoto Univ RISH and NII archives are academic/delayed). Free alternative: Open-Meteo mirror in section 8.

## 6. Taiwan (CWA, AWS bucket `cwaopendata`, region ap-northeast-1)
- Anonymous HTTPS: `https://cwaopendata.s3.ap-northeast-1.amazonaws.com/<key>`, list with `https://cwaopendata.s3.amazonaws.com/?prefix=Model/&delimiter=/` (works, XML ListObjectsV2). Registry: https://registry.opendata.aws/cwa_opendata/ ; licence http://data.gov.tw/license (Taiwan Open Government Data licence, attribute CWA). Each data id has `.json` and `.xml` metadata next to the data.
- **M-A0064-nnn.grb2 (3 km WRF, Taiwan region)**: `ForecastHour` nnn, files 000..084 step 6 (15 files; 12Z init today, written 16:53-18:10Z, i.e. +4.9..+6.2 h), 179 MB each (all levels). Walked all 57 messages of `M-A0064-006` by ranged requests: messages 61-68 are surface fields (sp, APCP 0-6 h accumulation, 2t, 2d, 2sh, 2r, 10u, 10v); no gust, cloud cover, ptype, or reflectivity (no REFC seen; UNVERIFIED for 3-D tail). **Grid template 3.30 Lambert conformal**: 1158 x 673, dx=dy=3000 m, Latin1 10, Latin2 40, LoV 120, LaD 10, first point (14.02224, 105.25), jScansPositively=1, shape 6, flags 48 (u/v earth-relative: bit 5 clear). **Packing 5.0 simple, 24 bits**. Extract surface messages via Range requests (`/M-A0064-006.grb2` supports it); saved `cwa/M-A0064-006.sfc_msgs61-68.grb2` (18.7 MB).
- **M-A0061 (15 km regional WRF)**: 661 x 385, first point lat -5.69, lon 78.03 to lat 43.29, lon -179.55 (grid type not inspected, UNVERIFIED), 58 MB. **M-A0060**: global 0.25 deg, 1440x721, lon 0..359.25, lat 90..-90 (per metadata "west to east then north to south"), 62 MB each, steps through 384 h (52 files); packing and variables UNVERIFIED (not downloaded).
- **Radar raw**: `Observation/O-A0059-001.json`: composite reflectivity, `GridDimensionX=921`, `Y=881`, resolution 0.0125 deg, lower-left (115.0 E, 18.0 N), x west to east then y south to north, dBZ, `-99` = no echo, `-999` = outside range/QC removed, **CRS TWD67** (a few hundred metres off WGS84), `content` is one comma-separated string of 811401 floats in scientific notation. 8.9 MB per frame, cadence 10 min; frame 18:20Z was published 18:27Z. Also coloured `O-A0058-001/002/003.png` (3600x3600 images, lon 118-124, lat 20.5-26.5). Sample: `cwa/O-A0059-001.json`.
- Gotcha: S3 `Last-Modified` of model files spans 1.5 h; wait for the highest-hour file.

## 7. South America

### 7.1 Argentina SMN (AWS `smn-ar-wrf`, region us-west-2, licence CC BY 2.5 AR)
- Registry id is `smn-ar-wrf-dataset` (the bare `smn-ar-wrf` registry page 404s, but the bucket `s3://smn-ar-wrf` is correct). Docs: https://odp-aws-smn.github.io/documentation_wrf_det . Anonymous list: `https://smn-ar-wrf.s3.amazonaws.com/?list-type=2&prefix=DATA/WRF/DET/2026/11/06/12/`.
- Layout `DATA/WRF/DET/YYYY/MM/DD/HH/WRFDETAR_{01H|10M}_YYYYMMDD_HH_{fff}.nc`. 01H files: hourly, one time per file, fff 000..072 (73 files). 10M files: PP only at 10-min steps (6 per file, e.g. `_003.nc` holds hours 3 +10 min steps). Runs present today: 00, 06, 12 (docs say 00 and 12).
- Latency: 00Z run fully listed with LastModified 02:28-02:49Z (+2h49); 06Z 08:25-08:40Z. The **12Z run was still partial at 18:30Z** (49 of 149 files, scattered fff, LastModified 14:26..18:12): check that `_072` exists before using a run.
- Example (200): `https://smn-ar-wrf.s3.amazonaws.com/DATA/WRF/DET/2026/11/06/12/WRFDETAR_01H_20261106_12_006.nc` (37 MB; `_10M_` 10 MB).
- Container: HDF5 (netCDF-4, magic `89 48 44 46 0d 0a 1a 0a`), CF-1.8. Dims time=1, y=1249, x=999; float32 `(time,y,x)`, `_FillValue 1e20`; chunk = whole field `[1,1249,999]`, zlib level 4 with shuffle (except `Freezing_level` uncompressed). Variables in 01H: `T2` (calibrated 2 m T, degC), `HR2` (2 m RH %), `magViento10` (calibrated 10 m speed m/s), `dirViento10` (deg; earth-relative meteorological convention assumed, UNVERIFIED), `PP` (accumulated total precipitation mm, since run start presumed, UNVERIFIED), `PSFC` hPa, `Freezing_level` m, soil fields, accumulated radiation. **No dew point (derive from T2+HR2), no u/v, no gust, no cloud cover, no ptype, no snow, no reflectivity.**
- Projection: `Lambert_Conformal` grid-mapping var: lambert_conformal_conic, standard parallels -35, -35, central meridian -65, origin lat -35, earth radius 6370000 m; x from -1.996e6 to 1.996e6, y -2.496e6..2.496e6, 4000 m; 2-D `lat`/`lon` arrays included. Corners: SW (-54.39,-94.33), NE (-11.65,-47.97) (global attrs: min_lat -56.85, max_lat -11.65, min_lon -94.33, max_lon -35.67).
- Samples: `smn/WRFDETAR_01H_20261106_12_006.nc`, `smn/WRFDETAR_10M_20261106_12_003.nc`. Attribution: cite SMN Argentina (exact required wording UNVERIFIED; CC BY 2.5 AR needs attribution).

### 7.2 Brazil CPTEC/INPE
- Base `https://dataserver.cptec.inpe.br/dataserver_modelos/wrf/ams_07km/brutos/YYYY/MM/DD/00/` (the older `ftp.cptec.inpe.br/modelos/tempo/WRF/ams_07km/brutos/` redirects here). Only the **00Z** run is published. Per valid time: `WRF_cpt_07KM_2026110600_2026110612.grib2` (+ `.inv` wgrib-style index with byte offsets, `.idx`, `.ctl`), 204 MB for all levels. Valid times hourly to at least 2026111312 (180 h). Range requests work using the `.inv` offsets (I downloaded 8 surface messages, 5.3 MB).
- Latency: f000 file 05:20Z, f012 file 05:34Z for the 00Z run (about +5h20..+5h35).
- Verified 200: `https://dataserver.cptec.inpe.br/dataserver_modelos/wrf/ams_07km/brutos/2026/11/06/00/WRF_cpt_07KM_2026110600_2026110612.grib2`.
- Surface-ish fields in each file (inv lines): `TMP:2 m above ground`, `DPT:2 m`, `RH:2 m`, `SPFH:2 m`, `UGRD/VGRD:10 m`, `GUST:surface`, `APCP:surface:0-12 hour acc fcst` (accumulated since run start, kg/m2, max 143), `ACPCP`, `NCPCP`, `TCDC:entire atmosphere`, `LCDC/MCDC/HCDC`, `REFC:entire atmosphere` (dB, -20..46), `REFD:1000 m` and `4000 m above ground`, `WEASD:surface` (snow water equivalent on ground), `VIS`, `CAPE`. No ptype or snowfall-rate/accumulation.
- Grid: **template 3.0 regular lat-lon**, 1019 x 1081, di=dj=0.07 deg, first (-57.90137, 269.32605), last (17.69863, 340.58605) i.e. lon -90.67..-19.41, jScansPositively=1, shape 6, flags 48. **Packing 5.3 (complex + spatial differencing)**: supported by our decoder today. Sample: `cptec/cptec_wrf7km_f012_sfc_subset.grib2`.
- Licence: not found on the server (UNVERIFIED); treat as attribution to INPE/CPTEC. Brazilian radar composite: not found key-free (REDEMET needs a key, see section 10).

## 8. Open-Meteo bucket on AWS (`s3://openmeteo`, anonymous, CC BY 4.0 per registry)
- Registry: https://registry.opendata.aws/open-meteo/ . Listing: `https://openmeteo.s3.amazonaws.com/?delimiter=/&prefix=data_spatial/`. Three trees: `data/` (time-series per model), `data_run/`, **`data_spatial/<model>/`** (full-grid files; use this). Models in `data_spatial` include `jma_gsm`, `jma_msm`, `cma_grapes_global`, `cmc_gem_gdps_15km`, `cmc_gem_hrdps`, `cmc_gem_rdps_10km`, `ecmwf_ifs025`, `ecmwf_aifs025_single`, `ncep_gfs025`, `dwd_icon*`, `meteofrance_arpege/arome`, `ukmo_*`, `metno_nordic_pp`, `ncep_hrrr_conus`, and more. `kma_gdps`, `kma_ldps`, `bom_access_global` exist under `data/` only (no `data_spatial/latest.json`, 404): not usable as full grids.
- Discovery: `data_spatial/<model>/latest.json` (completed flag, `reference_time`, `last_modified_time`, `valid_times`, `variables`, `crs_wkt` with BBOX). File: `data_spatial/<model>/YYYY/MM/DD/HH00Z/YYYY-MM-DDTHH00.om`, one file per valid time containing all variables.
- Example (200): `https://openmeteo.s3.amazonaws.com/data_spatial/jma_msm/2026/11/06/1200Z/2026-11-06T1500.om` (800 KB, saved `openmeteo/msm_2026-11-06T1500.om`).
- Format: Open-Meteo `.om` v3 (magic `4f 4d 03`), hierarchical, each variable an array [lat, lon] float32, chunk 32x32, compression `pfor_delta_2d_int16`, scale factors (temperature x20, wind x10, precipitation x10, RH/cloud x1, pressure x10), offset 0. Decodes with the python `omfiles` package (and the Rust crate of the same project; crate name/version UNVERIFIED). Also includes `crs_wkt`, `forecast_reference_time`, `valid_time`.
- JMA MSM: lat 22.4..47.6, lon 120..150, 505 x 481 (0.05 deg lat x 0.0625 deg lon), 79 valid times hourly from 12Z (78 h), variables `temperature_2m`, `relative_humidity_2m` (no dew point), `wind_u_component_10m`, `wind_v_component_10m` (earth-relative), `precipitation` (hourly mm presumed), `cloud_cover(+low/mid/high)`, `pressure_msl`, `shortwave_radiation`. No gust, ptype, snow, reflectivity. 12Z run `last_modified` 17:05Z (+5h05).
- JMA GSM: global 0.5 deg (lon -180..179.5), 06Z run last modified 15:31Z, 23 valid times to 2026-11-11 18:00Z; variables as MSM plus pressure levels.
- CMA GRAPES global: lat -89.9375..89.9375, lon -180..179.875 (0.125 deg), 41 valid times to +5 d (3-hourly presumed), variables include `temperature_2m`, `relative_humidity_2m`, `wind_*_10m`, **`wind_gusts_10m`, `precipitation`, `showers`, `snowfall`, `precipitation_type`, `cloud_cover`, `visibility`**; 06Z run published 12:28Z (+6h28). Upstream CMA licence for redistribution not verified (UNVERIFIED).
- Attribution: CC BY 4.0, "Weather data by Open-Meteo.com" style credit plus the original provider (JMA, CMA); exact required text UNVERIFIED. The bucket is a third-party derivative with lossy quantisation (0.05 degC, 0.1 mm); fine for display.

## 9. AWS/Google/Azure registries (weather-relevant, not probed unless noted)
From https://registry.opendata.aws/tag/weather/ (checked today): `smn-ar-wrf-dataset` (verified above), `cwa_opendata` (verified), `open-meteo` (verified), `ecmwf-forecasts` (global IFS open data, likely covered by the Europe agent), `dynamical-eccc-hrdps` (HRDPS as Icechunk Zarr by dynamical.org, UNVERIFIED), `dynamical-tj-nwp` (TJweather global-regional NWP, UNVERIFIED), `noaa-himawari` and `noaa-gk2a-pds` (Himawari-8/9 and KMA GK-2A geostationary satellite, UNVERIFIED), `wis2-global-cache-jma` (WIS2 cache; registry says "no restrictions, attribution requested", contents not inspected), `ideam-radares` (Colombian radar network, UNVERIFIED), `roa` (Rain over Africa, UNVERIFIED), `deafrica-chirps`, `australian-bureau-of-meteorology-barra-*` and `barpa` (reanalysis/projections, not real-time), `met-office-*` (UK, other agent), `nexrad-arco`, `fmi-radar` (Finland). Google/Azure registries were not searched.

## 10. Key-gated or unusable (name + sign-up)
- Australia BoM ACCESS (G/GE/C/R): registered users only via `ftp-reg.cloud.bom.gov.au` / SFTP (https://www.bom.gov.au/catalogue/data-feeds.shtml, registration for "Registered User Services"); anonymous FTP only has samples. BoM radar images are web PNGs (not raw) with copyright terms that restrict redistribution; not adopted.
- Korea KMA: API Hub https://apihub.kma.go.kr/ (key required; site did not even answer plain HTTPS from here) and https://data.kma.go.kr/ (sign-up).
- Japan JMA GSM/MSM GRIB2: JMBSC (https://www.jmbsc.or.jp/) registration/paid; Open-Meteo mirror is the key-free path.
- India IMD/NCMRWF: https://mausam.imd.gov.in/ serves images; NCMRWF NCUM gridded data via https://data.ncmrwf.gov.in/ (registration); no key-free grid or raw radar found (UNVERIFIED, limited probing).
- New Zealand: MetService publicData radar path 404; MetService Data Portal (https://www.metservice.com/ , API keys) and NIWA (https://data.niwa.co.nz/) need sign-up. No key-free grids found (UNVERIFIED).
- Hong Kong HKO: key-free JSON point API at `https://data.weather.gov.hk/weatherAPI/opendata/weather.php?dataType=rhrread&lang=en` (200) and docs https://data.weather.gov.hk/weatherAPI/doc/HKO_Open_Data_API_Documentation.pdf; gridded nowcast CSV URL I tried returned 301 (not followed). Area is tiny; not pursued.
- Mexico SMN/CONAGUA: `https://smn.conagua.gob.mx/tools/GUI/webservices/?method=1` returns key-free gz (`DailyForecast_MX.gz`, per-city point forecasts, updated 18:31Z); no gridded model or raw radar found.
- South Africa SAWS: no open grid or radar found (UNVERIFIED).
- Brazil radar (REDEMET API key, https://api-redemet.decea.mil.br/), CPTEC BAM global (`/modelos/tempo/BAM/` exists, not fetched).

## 11. Decoder and projection implications
1. **JPEG2000 (5.40)** is needed for every ECCC model file (GDPS, RDPS, HRDPS). The current `grib` crate path supports 5.0/5.2/5.3/5.41 only. Add a JPEG2000 decoder (e.g. openjpeg via a Rust binding; choice and feature flags UNVERIFIED) or pre-convert. Decoded values are scaled integers (bits per value 7-24).
2. **Rotated lat-lon (3.1)** for RDPS/HRDPS: implement the southern-pole rotation (lat -36.08852/lon 245.305142 for HRDPS; -31.758312/267.597031 for RDPS) and **rotate u/v to earth-relative** (flags 56). The first/last points in the file are rotated-frame coordinates; true lon wraps through 180 for RDPS.
3. Lambert 3.30 already supported (CWA 3 km), packing 5.0 24-bit simple.
4. Regular lat-lon 3.0 + packing 5.3 (CPTEC) already supported.
5. Non-GRIB: NetCDF4/HDF5 (SMN), `.om` (Open-Meteo), JSON text (CWA radar), palette PNG tiles (JMA), WMS PNG (ECCC radar) each need a small dedicated reader.
6. Accumulation semantics: ECCC `Precip-Accum`/`APCP_Sfc` and CPTEC `APCP` are accumulated from run start (step range 0-hhh); ECCC also offers 1 h and 3 h accumulations and `WEASN*` liquid-equivalent snow. CWA M-A0064 `APCP` 0-6 per 6-hourly file (UNVERIFIED whether since run start or window; the file at step 6 showed 0-6).
