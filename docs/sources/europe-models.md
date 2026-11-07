# European and global open NWP sources (key-free): verified facts

Research date: 2026-11-06 (all fetches made that day, 18:15-19:00 UTC). Everything below was fetched, not recalled, unless marked UNVERIFIED.
Tooling: eccodes 2.50 (Python wheel) for GRIB inspection, netCDF4/h5py for NetCDF. Sample files are in
`/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/europe-models/` (see the end of this file).
All requests used `User-Agent: weather-map-dev`, at most about 2 requests per second per host.

## 0. Summary and recommendation

| Rank | Source | Role | Why |
|---|---|---|---|
| 1 | ECMWF IFS open data, 0.1 degree (new 2026-10-20), fallback 0.25 | Global base | Only free global model at ~9 km. Has precip type, precip rate, gust, total cloud, snow. CC BY 4.0. Per-field byte-range via JSON `.index`. Needs a CCSDS (5.42) decoder. |
| 2 | DMI HARMONIE DINI (2 km) and IG (2 km) | Regional overlay: UK, Ireland, Benelux, Denmark, Norway south of 70N, Iceland, N Atlantic; IG = Greenland, Iceland, Svalbard | Grid 3.30 Lambert + packing 5.0: both already supported by our decoder. Key-free S3 bucket with a wgrib2-style `.index` for byte ranges. Catch: winds are grid-relative, files are 0.5-0.6 GB each. |
| 3 | DWD ICON-D2 (2.2 km), regular-lat-lon files | Regional overlay: Germany, Alps, Benelux, Poland, Czechia, Denmark | Grid 3.0 + packing 5.0 (with bitmap): already supported. One variable per file, bz2. Has `ww` (present weather code), rain/snow/graupel rates, simulated reflectivity (`dbz_cmax`), 15-minute output for precip fields. |
| 4 | Météo-France AROME 0.025 (and 0.01 for wind/T/RH/rain/snow/reflectivity), plus AROME Outre-Mer (5 overseas domains) | Regional overlay: France, Iberia N, Italy N, Alps, S England; overseas: Antilles, Guyane, Indian Ocean (Réunion/Mayotte), New Caledonia, Polynesia | Only free high-resolution model for the tropics and southern hemisphere overseas territories. All CCSDS (5.42). No index: you must walk message headers. |
| 5 | MET Norway MEPS (control member, 2.5 km) and AROME-Arctic (2.5 km) | Nordic and Arctic | NetCDF over OPeNDAP only (no file download). Covers Finland, N Norway, Svalbard where DINI does not. Wind is grid-relative. Medium effort. |
| 6 | Met Office UKV 2 km / Global 10 km | UK | NetCDF4 per variable per step, simple. But CC BY-SA 4.0 (share-alike) and DINI/ICON-EU already cover the UK. Low priority. |
| 7 | DWD ICON-EU (6.5 km), DWD ICON global (13 km), ARPEGE, AIFS, FMI, MeteoSwiss, GeoSphere | Not needed | Redundant with IFS 0.1 degree or with the overlays above (see sections). |

Global base choice: ECMWF IFS 0.1 degree. ICON global is a worse base: icosahedral only (no regular-lat-lon product), 13 km, no precip-type field (only `ww`), needs the CLAT/CLON files and a scatter-to-grid step.

The single biggest enabler is a **CCSDS / AEC (template 5.42) decoder**: IFS, AIFS, ICON global, ICON-EU, every Météo-France product use it. Whether the pure-Rust `grib` crate we use decodes 5.42 is UNVERIFIED (check before committing to the design). ICON-D2, DMI HARMONIE and FMI need nothing beyond what we already have.

## 1. DWD ICON (https://opendata.dwd.de/weather/nwp/)

Top-level directories seen: `icon/ icon-d2/ icon-d2-eps/ icon-eps/ icon-eu/ icon-eu-eps/ snow4/ v1/`. There is **no `icon-d2-ruc`** (or any rapid-update variant) on the server today (the path returns an empty listing). ICON-D2 itself already produces 15-minute output for some fields (below).

### 1.1 Layout and naming

`/weather/nwp/<model>/grib/<HH>/<var>/<file>.grib2.bz2` where `<HH>` is the run hour and `<var>` is a lowercase variable directory. One variable and one forecast step per file (a few fields carry several messages, see D2). Every file is individually bzip2-compressed. Apache autoindex listing (HTML, `Last-Modified` shown in UTC, sizes in bytes). No index files, none needed.

| Model | Example file name (verified 200 today) |
|---|---|
| icon (global) | `icon/grib/12/t_2m/icon_global_icosahedral_single-level_2026110612_003_T_2M.grib2.bz2` |
| icon-eu | `icon-eu/grib/12/t_2m/icon-eu_europe_regular-lat-lon_single-level_2026110612_003_T_2M.grib2.bz2` |
| icon-d2 | `icon-d2/grib/12/t_2m/icon-d2_germany_regular-lat-lon_single-level_2026110612_003_2d_t_2m.grib2.bz2` |

Notes:
- Tokens: global and EU use UPPERCASE short names (`T_2M`, `TOT_PREC`, `VMAX_10M`, `WW`); D2 uses `2d_` + lowercase (`2d_t_2m`).
- Step is 3 digits, hours (`003`). In D2 the 15-minute messages are inside the hourly file (see 1.4).
- The run time in the file name is `YYYYMMDDHH`.
- Discover the newest complete run: list `/weather/nwp/<model>/grib/` (directory mtimes are NOT reliable, a run folder can show a stale date). Instead list `<HH>/t_2m/` for the candidate hours and take the latest run whose listing has the full step count (see table) or whose last file exists.

### 1.2 Grids offered

- **icon (global)**: icosahedral ONLY. The global `t_2m` directory contains only `*_icosahedral_*` files (checked the whole listing: no regular-lat-lon file). Grid template **3.101** (`unstructured_grid`), 2,949,120 cells (about 13 km, R03B07), `numberOfGridInReference=1`, `numberOfGridUsed=26`, `uuidOfHGrid = a27b8de618c411e4820ab5b098c6a5c0`, earth shape 6.
  - Cell centres: separate time-invariant files in `icon/grib/<HH>/clat/` and `.../clon/`: `icon_global_icosahedral_time-invariant_2026110612_CLAT.grib2.bz2` (1.26 MB bz2, 2.57 MB raw) and `..._CLON.grib2.bz2` (1.40 MB bz2, 2.63 MB raw). Each is one GRIB2 message, 2,949,120 values, template 3.101, packing **5.42**, 16 bits per value. Values are **degrees** (eccodes `tlat`/`tlon`, units "Degree N"/"Degree E"; lat range -89.913..89.915, lon range -180..180). Quantisation: lat binary scale -8 (step 0.0039 deg, ~430 m), lon binary scale -7 (step 0.0078 deg, ~870 m). Same `uuidOfHGrid` as the data, so cache once and key by uuid; the file is re-published in every run folder.
  - Data array order equals the CLAT/CLON array order (no coordinates in the data message).
  - Wind: components are earth-relative (east/north) (ICON convention; no grid rotation exists on the sphere). Flag octet not present for 3.101. Treat as UNVERIFIED but there is no alternative frame.
- **icon-eu**: `regular-lat-lon`, template **3.0**, 1377 x 657, spacing 0.0625 deg, lat 29.5 to 70.5, lon 336.5 (=-23.5) to 62.5, scanning flags 64 (south to north, west to east, `jScansPositively=1`), earth shape 6 (sphere 6371229 m), `resolutionAndComponentFlags=48` (increments given, winds earth-relative), no bitmap, 904,689 points. There is no rotated-pole file in the listing (the `rlat`/`rlon` directories exist, only coordinate helper fields).
- **icon-d2**: both `icosahedral` (542,040 cells, with `clat/clon` directories, uuid `c6b12daa91ad64045b26c1b6452a2a20`) and `regular-lat-lon`. Use regular: template **3.0**, 1215 x 746, spacing 0.02 deg, lat 43.18 to 58.08, lon 356.06 (=-3.94) to 20.34, scan 64, flags 48. Package has a **bitmap** (section 6): 754,862 of 906,390 points are valid; invalid = 9999 in eccodes output. The domain is the icosahedral footprint resampled onto a rectangle, so the corners are missing.

### 1.3 Packing templates seen (section 5)

| Model | Template 5 | Template 3 |
|---|---|---|
| icon global | **5.42 CCSDS** (bitsPerValue 16) | 3.101 |
| icon-eu regular | **5.42 CCSDS** | 3.0 |
| icon-d2 regular | **5.0 simple** (16 bits, bitmap) | 3.0 |

### 1.4 Runs, horizon, step, latency (all measured from `Last-Modified` on 2026-11-06)

| Model | Runs/day | Horizon and steps | First file after run | Last file after run |
|---|---|---|---|---|
| icon | 4 (00, 06, 12, 18) | 00/12: 180 h (hourly to 78 h, 3-hourly to 180 h; 113 files per variable). 06/18: 120 h (93 files) | +2 h 39-42 | 12Z: +3 h 25 (15:25 UTC); 06Z: +3 h 12 |
| icon-eu | 8 (every 3 h) | 00/06/12/18: 120 h (93 files, hourly to 78 h then 3-hourly). 03/09/15/21: 30 h (34 files) | +2 h 40-44 | 12Z (120 h): +3 h 38; 15Z (30 h): +2 h 54 |
| icon-d2 | 8 (every 3 h) | 48 h hourly (49 files per grid per variable; 98 files incl. both grids) for the 12Z and 15Z runs I checked | +0 h 44 | +1 h 20 (12Z done 13:20, 15Z done 16:20) |

- ICON-D2 sub-hourly: files for `tot_prec`, `snow_gsp`, `prs_gsp`, `dbz_cmax` hold **4 GRIB messages** (hh:00, +15, +30, +45 min). `stepRange` of the extra messages is in minutes (`195m`, `210m`, `225m` in the `003` file; `0m-375m` for accumulations). `t_2m`, `ww`, `clct`, `vmax_10m` are single-message files (checked).
- Newest-run discovery for the pipeline: for each model request the directory listing of `<HH>/t_2m/` for the last 2-3 run hours and pick the largest run time where the last expected step is present.

### 1.5 File sizes (bz2 as served; raw in parentheses)

| File | Size |
|---|---|
| global T_2M one step | 3.3 MB (3.3 MB raw; CCSDS data does not recompress) |
| global TOT_PREC 6 h | 2.2 MB bz2; VMAX_10M 4.1 MB |
| icon-eu T_2M | 1.0 MB (1.0 MB raw) ; U_10M 1.2 MB ; TOT_PREC 0.52 MB ; WW 0.25 MB (1.0 MB raw) |
| icon-d2 regular T_2M | 0.73-1.0 MB bz2 (1.6 MB raw); TD_2M 1.05 MB; U_10M 1.0 MB; VMAX_10M 1.1 MB; TOT_PREC 0.60-0.86 MB (4 msgs raw 6.5 MB); CLCT 0.67 MB; WW 0.05 MB; DBZ_CMAX 2.7 MB (4 msgs); PRS_GSP 0.04-0.06 MB |

Fetching a single field is natural (one file per field-step). bz2 forbids partial reads, but files are small.

### 1.6 Variables we need (exact tokens, units, semantics)

Directory name / file token pairs are for D2 (lowercase) and EU/global (uppercase).

| Need | D2 | EU | Global | Units / semantics |
|---|---|---|---|---|
| 2 m temperature | `t_2m` | `T_2M` | `T_2M` | K, instantaneous (eccodes `2t`) |
| 2 m dew point | `td_2m` | `TD_2M` | `TD_2M` | K, instantaneous (`2d`). `relhum_2m` also exists (%) |
| 10 m wind | `u_10m`, `v_10m` | `U_10M`, `V_10M` | same | m/s, earth-relative, instantaneous |
| 10 m gust | `vmax_10m` | `VMAX_10M` | `VMAX_10M` | m/s, `max_i10fg`, **max over the previous output interval** (stepRange 5-6 at step 6) |
| Total precipitation | `tot_prec` | `TOT_PREC` | `TOT_PREC` | kg m-2 (= mm), **accumulated since run start** (stepRange `0-6`; D2 sub-hourly `0m-375m`). Per-step amount = difference of consecutive steps |
| Rain / snow / graupel accumulations | `rain_gsp`, `snow_gsp`, `grau_gsp`, `rain_con`, `snow_con` | `RAIN_GSP`, `SNOW_GSP`, `RAIN_CON`, `SNOW_CON` | same | kg m-2, accumulated since run start (grid-scale and convective). `snow_gsp` = `lsfwe` (snowfall water equivalent) |
| Precip rates | `prr_gsp` (rain), `prs_gsp` (snow, `lssfr`), `prg_gsp` (graupel) | none | none | kg m-2 s-1, instantaneous, D2 only. Instantaneous kind = valid at that minute |
| Precipitation type | `ww` | `WW` | `WW` | WMO present weather code 0-99 (observed 0..96/86). No ptype flag fields. Classify via ww (e.g. 60-69 rain/drizzle/freezing, 70-79 snow, 68/69 sleet, 56/57 and 66/67 freezing) or from rain/snow/graupel rates (D2) or accumulations |
| Total cloud | `clct` | `CLCT` | `CLCT` | percent 0-100, instantaneous. Also `clch/clcm/clcl` |
| Snow | `h_snow` (depth, m), `snowc` (D2 only) | `H_SNOW` | `H_SNOW` | `sde` snow depth in metres. Observed EU max 40.0 (glacier cells), so clip or ignore. For new snowfall use `snow_gsp`+`snow_con` accumulations |
| Simulated radar | `dbz_cmax` (column max), `dbz_850`, `dbz_ctmax`, `echotop` | none | none | dBZ. `-150` is the "no echo" floor. 15-min output |

## 2. ECMWF open data (https://data.ecmwf.int/forecasts/)

Access: plain HTTPS, no key. The server honours `Range`. (An S3 interface is advertised on the portal; I did not test it. The base path is the same.) Portal limit: 2000 simultaneous connections overall; each parallel connection counts as a session.

New this autumn:
- **0.1 degree (about 9 km) IFS** has been published since **2026-10-20** next to the 0.25 degree. Directory `ifs/0p1/`. It is released **2 h after** the 0.25 degree data. AIFS stays 0.25 only.
- Packing precision is now per parameter (set 2026-10-20), so bits per value vary by field (e.g. `2t` at 0.1 deg is 9 bits).

### 2.1 Layout and naming

```
/forecasts/<YYYYMMDD>/<HH>z/ifs/0p25/oper/<YYYYMMDD><HH>0000-<step>h-oper-fc.grib2      (+ .index)
/forecasts/<YYYYMMDD>/<HH>z/ifs/0p1/oper/<YYYYMMDD><HH>0000-<step>h-oper-fc-sfc.grib2   (+ .index)   surface/soil
/forecasts/<YYYYMMDD>/<HH>z/ifs/0p1/oper/<YYYYMMDD><HH>0000-<step>h-oper-fc-pl.grib2    (+ .index)   pressure levels
/forecasts/<YYYYMMDD>/<HH>z/aifs-single/0p25/oper/<YYYYMMDD><HH>0000-<step>h-oper-fc.grib2 (+ .index)
```
Also `enfo`, `waef`, `wave`, `aifs-ens`: ignored. Concrete URL that returned 200 today:
`https://data.ecmwf.int/forecasts/20261106/00z/ifs/0p1/oper/20261106000000-3h-oper-fc-sfc.grib2` (92.4 MB).

The directory pages are HTML (parse `href`s). Discover the newest run by listing `/forecasts/<date>/` (shows 00z/06z/12z/18z as they appear) and then `<HH>z/ifs/` (the `ifs` folder appears only when the run is released: at 18:22 UTC the 12z folder had `aifs-*` only).

### 2.2 `.index` format and per-field fetch

JSON Lines, one object per GRIB message, with byte offset and length. Example (0.1 deg, step 3, 2t):
`{"domain": "g", "date": "20261106", "time": "0000", "expver": "0001", "class": "od", "type": "fc", "stream": "oper", "step": "3", "levtype": "sfc", "param": "2t", "_offset": 85186067, "_length": 1365139}`
Keys: `domain,date,time,expver,class,type,stream,step,levtype,param`, plus `levelist` for `sol`/`pl`/`ml`. Fetch `Range: bytes=offset-(offset+length-1)`. Verified: 12 parameters fetched this way (24 MB) and decoded. `0.1 deg` field size 1.4 MB (2t) to 3.6 MB (10u). Messages are in arbitrary order in the file; always go through the index.

### 2.3 Resolution, steps, runs, latency

| Stream | Grid | Runs | Steps | Latency (Last-Modified minus run time) |
|---|---|---|---|---|
| IFS 0.25 | 1440 x 721 | 00, 06, 12, 18 | 00z/12z: 0-144 h every 3 h, then 150-360 h every 6 h (verified list for 00z). 06z/18z: 0-144 h every 3 h (verified: 06z list ends 144; 18z 150h is 404) | 00z: +7 h 34; 06z: +6 h 27; 18z: +6 h 27. 12z: not yet out at 18:22 UTC (expected about +7 h 34, UNVERIFIED) |
| IFS 0.1 | 3600 x 1801 | same | same step set (00z list verified to 360 h; 06z verified to 90 h at least) | 0.25 latency + 2 h: 00z +9 h 34, 06z +8 h 27, 18z +8 h 27 |
| AIFS single 0.25 | 1440 x 721 | 4 | every 6 h to 360 h (00z list: 0, 6, 12, ..., 360) | 0 h file +5 h 45; 00z 360 h file +7 h 34 |

File sizes: 0.25 combined 99 MB per step; 0.1 `sfc` 79-98 MB, `pl` separate; AIFS about 57-64 MB per step.

### 2.4 Grid and packing (all identical in structure)

Template **3.0**, `regular_ll`, earth shape 6 (sphere 6371229), flags 48 (winds earth-relative), scan mode 0 (north to south, west to east). 0.1 deg: Ni 3600, Nj 1801, first point lat 90 / lon **180.0**, last point lat -90 / lon 179.9, spacing 0.1. 0.25 deg / AIFS: 1440 x 721, lon 180.0 to 179.75. **Gotcha:** the first column is longitude 180 E (= -180), i.e. the array starts at the antimeridian, not Greenwich, and includes both poles. Packing is **5.42 CCSDS** on every message I decoded. No bitmap.

### 2.5 Parameters (short names from the index)

Surface (identical set for 0.25 and 0.1, step >0): `2t, 2d, 10u, 10v, 10fg, 100u, 100v, tp, tprate, sf, sd, rsn, ptype, tcc, lcc, mcc, hcc, tcwv, tcw, msl, sp, skt, cp, mucape, ro, ssr, ssrd, str, strd, ttr, ewss, nsss, asn, mx2t3, mn2t3, sithick, lsm, zos, tciw, tclw, tcrw, sve, svn`, soil `sot/vsw` at levels 1-4. Pressure levels: `d gh q r t u v vo w z`.

| Need | IFS shortName | Units and semantics (verified from decoded messages) |
|---|---|---|
| 2 m T | `2t` | K |
| 2 m dew point | `2d` | K |
| 10 m wind | `10u`, `10v` | m/s, instantaneous, earth-relative |
| 10 m gust | `10fg` | m/s, max since previous post-processing; at step 3 `stepRange` is `2-3` (so the last hour) |
| Precip amount | `tp` | **metres** of water (not mm), accumulated from run start (stepRange `0-3`, `0-6`) |
| Precip rate | `tprate` | kg m-2 s-1, instantaneous |
| Precip type | `ptype` | instantaneous, WMO code table 4.201, observed values 0..12 (0 none, 1 rain, 3 freezing rain, 5 snow, 6 wet snow, 7 rain-snow mix, 8 ice pellets, 11 drizzle, 12 freezing drizzle; the name mapping is from the WMO table, UNVERIFIED against the file) |
| Total cloud | `tcc` | IFS: fraction 0-1 (units "(0 - 1)"). **AIFS: percent 0-100** (gotcha, decoded max 100) |
| Snow | `sf` (snowfall, m water equivalent, accumulated from start), `sd` (depth, m of water equivalent, instantaneous), `rsn` (density kg m-3; depth_m = sd * 1000 / rsn) | |
| Radar reflectivity | none | |

AIFS single available params (index at 6h): `2t 2d 10u 10v 100u 100v tp cp sf tcc lcc mcc hcc msl sp skt ssrd strd tcw fscov rowe sot vsw` and pressure-level fields. **No `10fg`, no `ptype`** in AIFS.

### 2.6 Licence

CC BY 4.0 plus the ECMWF Terms of Use ("may be redistributed and used commercially, subject to appropriate attribution"). Attribution: credit ECMWF as the source (suggested text: "Contains ECMWF forecast data, licensed CC BY 4.0, source: ECMWF"; the exact wording on their "appropriate attribution" page was not retrieved, UNVERIFIED). Their page also notes access "may be subject to Service Charges" for ECMWF-arranged delivery, not relevant to HTTPS download.

## 3. Météo-France open data

Fully key-free. The data.gouv.fr dataset pages point to a public S3-compatible bucket on OVH:

`https://meteofrance-pnt.s3.rbx.io.cloud.ovh.net/pnt/<RUN>/...` where `<RUN>` = `2026-11-06T12:00:00Z`. Anonymous `GET` works. Anonymous S3 `ListObjects` also works, e.g. `https://meteofrance-pnt.s3.rbx.io.cloud.ovh.net/?prefix=pnt/2026-11-06T12:00:00Z/arome/001/SP1/&max-keys=100` returns XML with `LastModified` and `Size`. Run folders are listed with `?prefix=pnt/2026-11-06&delimiter=/`. (The older `portail-api.meteofrance.fr` is key-gated and not needed.) Range requests work (HTTP 206, used on 20+ files). **No index files exist** for GRIB (the listing contains only `.grib2`).

The public technical description (PDF, "Descriptif technique des paquets de données modèle AROME", 2025-04-01, saved under `docs/`) lists the packages.

### 3.1 AROME France

- Runs: 8 per day (00, 03, 06, 09, 12, 15, 18, 21). Horizon 0-51 h hourly. 12Z run: 52 files per package, complete about +3 h 52 (`051H` at 15:53 UTC).
- Domain "EURW1S": 55.4N to 37.5N, 12W to 16E. Grid template **3.0**, flags 48 (earth-relative winds), scan mode 0 (north to south, west to east), earth shape 6, packing **5.42 CCSDS**, bitmap present. The native domain is trapezoidal, so a rectangle with many missing values: 83% valid (4,160,519 of 5,016,591 at 0.01). Missing values differ per parameter.
- Naming:
  - 0.01 deg (`001`, 2801 x 1791, lon first 348.0 = -12, last 16.0): one hourly file per package, `arome/001/<PKG>/arome__001__<PKG>__<HH>H__<RUN>.grib2` (HH 2 digits, `00H`..`51H`).
  - 0.025 deg (`0025`, 1121 x 717, same bbox): 6-hour packs `arome/0025/<PKG>/arome__0025__<PKG>__00H06H__<RUN>.grib2` (groups 00H06H, 07H12H, 13H18H, 19H24H, 25H30H, 31H36H, 37H42H, 43H48H, 49H51H).
- Concrete URL that returned 200:
  `https://meteofrance-pnt.s3.rbx.io.cloud.ovh.net/pnt/2026-11-06T12:00:00Z/arome/001/SP1/arome__001__SP1__03H__2026-11-06T12:00:00Z.grib2` (23.2 MB).
- Sizes: 0.01 deg SP1 about 23 MB per hour, SP2 17 MB, SP3 3.5 MB, HP1 about 75 MB; 0.025 deg SP1 56 MB per 6-hour group, SP2 41-46 MB, SP3 58-61 MB, IP*/HP* hundreds of MB.
- Latency (12Z run, `LastModified` minus run time): 0.01 SP1 `00H` +3 h 02, `11H` +3 h 12, `47H` +3 h 54; 0.025 SP1 `00H06H` +3 h 06, last group +3 h 51.
- **Per-field fetch without an index**: within a 0.025 6-hour file, messages are grouped by parameter (all 7 steps of 10wdir, then all 7 of 10si, ...), so one parameter is a contiguous byte range. The offset must be found by walking message headers: read 16 bytes of each message (section 0 gives the 8-byte total length) with a ranged GET and hop. I did this on a full 0.025 SP1 file (97 messages, one request each) and got the offsets. So per-field fetch is possible but costs about one request per message to build the map (cache it per run and package; offsets are identical across variables only by chance). The 0.01 hourly files are variable-major too but small enough (23 MB) to just download.

Package contents actually seen (decoded) and what we need:

| Need | 0.01 deg | 0.025 deg | Units / semantics |
|---|---|---|---|
| 2 m T | SP1 `2t` | SP1 `2t` | K |
| 2 m humidity | SP1 `2r` (RH %) | SP1 RH (cat 1 num 1) | %. 0.01 has no dew point; derive from T and RH. 0.025 SP2 has `2d` (dew point K) and `2sh` |
| 10 m wind | SP1 `10u`, `10v` | SP1 `10u`, `10v` plus `10si`/`10wdir` (speed, direction) | m/s, earth-relative (flags 48) |
| 10 m gust | SP1 `max_10efg`, `max_10nfg` (gust components, max over last hour: stepRange `2-3`) | SP1 `max_i10fg` (gust speed, stepRange `0-1`, `1-2`, ...) plus gust components | m/s |
| Precipitation | SP2 rain accumulation `tirf` (PDF name `EAU`), `tsnowp` (`NEIGE`), `tgrp` (`GRAUPEL`): sum the three | SP1 `tp` (accumulation 0-1, 0-2, ... from run start), also `tsnowp`, graupel in SP1 | kg m-2, **accumulated since run start** (stepRange `0-3`) |
| Precip type | none. Derive from the rain / snow / graupel accumulations (0.01: SP2; 0.025: SP1 `tsnowp`, graupel; SP2 `tirf`) | | |
| Total cloud | none (SP2: low `lcc`, medium `mcc`, high `hcc` fractions). Combine them (e.g. max) | SP1 cat 6 num 1 (total cloud cover; eccodes prints it "unknown"), steps 1..6 only (no step 0) | |
| Snow depth | none | none | |
| Radar reflectivity | SP2 discipline 0 category 16 number 193 (eccodes "unknown", PDF name `RFLCTVT_MAX`), values 0-25 observed at +3 h; units NOT verified (UNVERIFIED: dBZ versus mm/h) | HP3 `RFLCTVT` on 7 height levels (not fetched) | |
| Pressure | SP2 `sp` | SP1 prmsl | |

No `.index`, so identifiers are GRIB (discipline/category/number), not names: eccodes lacks Météo-France local entries for several (they show as "unknown").

SP3 at 0.01 deg is a synthetic satellite brightness temperature (template 4.32, cat 5 num 7; PDF: `BT`, canal 10.8).

### 3.2 AROME Outre-Mer (overseas, 0.025 deg)

`.../pnt/<RUN>/arome-om/<DOMAIN>/0025/<PKG>/arome-om-<DOMAIN>__0025__<PKG>__<HHH>H__<RUN>.grib2` (3-digit hours, one file per hour, **all SP1 fields in a single small file**, e.g. Polynesia `001H` 2.6 MB). Packages SP1, SP2, SP3, HP1-3, IP1-5.

| Domain code | Region | Grid (template 3.0, 0.025 deg, scan 0, 5.42) |
|---|---|---|
| `ANTIL` | Antilles/Caribbean | 945 x 529; lat 22.9 to 9.7, lon -75.3 to -51.7 |
| `GUYANE` | French Guiana | 419 x 317; lat 8.95 to 1.05, lon -56.75 to -46.3 |
| `INDIEN` | Réunion / Mayotte / Indian Ocean | 1395 x 899; lat -3.45 to -25.9, lon 32.75 to 67.6 |
| `NCALED` | New Caledonia | 521 x 491; lat -13.75 to -26.0, lon 158.5 to 171.5 |
| `POLYN` | Polynesia | 521 x 507; lat -12.6 to -25.25, lon -157.5 to -144.5 |

- Runs: folders for 00, 06, 12, 18Z; horizon 48 h hourly (49 files). INDIEN was missing in the 12Z listing and present at 06Z (UNVERIFIED whether INDIEN runs only twice a day).
- Latency: first file +6 h 25 (06Z and 12Z); full run about +7-8 h. Slow compared with France.
- SP1 content verified (POLYN `001H`): `10wdir, 10si, max_i10fg (0-1), prmsl, 10u, 10v, max_10efg, max_10nfg, 2t, 2r (RH %), tp (accum from start), total cloud cover (cat 6 num 1), tsnowp, ssrd, tgrp`. Same semantics as the 0.025 France SP1.
- The data.gouv.fr dataset listings (not the bucket) name the domains `NCALED0025`, `POLYN0025`, `GUYANE0025`, `CARAIB0025` (Antilles), `INDIEN0025`.

### 3.3 ARPEGE (global 0.25 deg and Europe 0.1 deg)

`.../pnt/<RUN>/arpege/025/<PKG>/arpege__025__<PKG>__000H024H__<RUN>.grib2` and `.../arpege/01/...`. Global 0.25: 1440 x 721, lat 90 to -90, lon 0 to 359.75 (starts at Greenwich). Europe 0.1: 741 x 521, lat 72 to 20, lon 328 (=-32) to 42. Packing 5.42 (seen). Packs of 24 h (0.25: 000H024H, 025H048H, 049H072H, 073H102H; 0.1: 12-hour packs 000H012H, ..., 097H102H); SP1 0.25 about 263 MB per 24-hour pack, SP2 0.25 about 428 MB. Runs seen for 00, 06, 12Z today (18Z not yet); horizon 102 h; 0.25 12Z SP1 packs arrive +3 h 14 to +3 h 38, 0.1 SP1 +2 h 32 to +3 h 10. I only inventoried the start of SP1 (prmsl, 10 m wind speed and direction, hourly): the rest of the parameter set is UNVERIFIED. Not recommended (IFS 0.1 deg is the better global, AROME the better regional).

### 3.4 Licence

Licence Ouverte 2.0 ("lov2" on data.gouv.fr, Etalab). Attribute the source (Météo-France) and the date of last update; keep the link to the licence. No automated-access restriction found in the dataset pages; no rate limit documented. The bucket is fast (23 MB in 2.5 s).

## 4. UK Met Office on AWS Open Data

Registry (`awslabs/open-data-registry`, `met-office-uk-deterministic.yaml`, `met-office-global-deterministic.yaml`): bucket **`met-office-atmospheric-model-data`**, region eu-west-2. Both models live there. Anonymous HTTPS and S3 listing work: `https://met-office-atmospheric-model-data.s3.eu-west-2.amazonaws.com/?list-type=2&prefix=...`. "Offered on a free, unsupported basis", 2-year rolling archive.

- Layout: `uk-deterministic-2km/<RUN>/<VALID>-PT<LEAD>-<variable>.nc` and `global-deterministic-10km/<RUN>/...`. `<RUN>` = `20261106T1200Z`. **Gotcha: the file name starts with the VALID time, not the run time**: `uk-deterministic-2km/20261106T1200Z/20261106T1800Z-PT0006H00M-temperature_at_screen_level.nc` (lead 6 h). One variable, one step per file. 200 today.
- UK runs: hourly folders exist for every hour of the day (24 runs/day): nowcast 12 h runs at 01, 02, 04, 05, 07, ... UTC; short 54 h runs at 00, 06, 09, 12, 18, 21; medium 120 h at 03, 15 (registry text). Hourly steps to 54 h then 3-hourly to 120 h; plus **15-minute files** for a 12-variable subset (temperature, dew point, wind speed/direction/gust, precipitation / rain / snow / hail rate, CAPE, pressure, visibility) seen for leads 0-11 h at least. Latency: UK 12Z run files start appearing at +3 h 30 (15:30 UTC) and then arrive as produced; global 12Z at +5 h 30 (17:30).
- Global: 4 runs (00, 06, 12, 18); 00/12 to 168 h, 06/18 to 67 h; hourly to 54 h, 3-hourly to 144 h, 6-hourly to 168 h.
- Format: NetCDF-4 (HDF5 signature `89 48 44 46 0d 0a 1a 0a` verified). Data variable float32, **zlib level 1, chunks 128 x 128, no shuffle, no scale/offset**, CF-1.7. `least_significant_digit` attribute present on variables (lossy rounding before compression). Sizes: UK single-level 0.09-2.1 MB (e.g. temperature 1.5 MB, precipitation rate 0.38 MB, snowfall 0.09 MB); global 1-9 MB; 3D files up to 62 MB (UK) / 226 MB (global).
- UK grid: 970 x 1042, 2 km, `lambert_azimuthal_equal_area`, lat_0 54.9, lon_0 -2.5, WGS84 ellipsoid (a 6378137, b 6356752.314), x from -1,158,000 to 924,000 m, y from -1,036,000 to 902,000 m, step 2000. Row 0 is south.
- Global grid: 2560 x 1920, regular lat-lon, lat -89.953125 to 89.953125 (step 0.09375, S to N), lon -179.9297 to 179.9297 (step 0.140625), sphere radius 6371229.
- Variables (UK 6 h listing): `temperature_at_screen_level` (`air_temperature` K), `temperature_of_dew_point_at_screen_level`, `relative_humidity_at_screen_level`, `wind_speed_at_10m` (m/s) and `wind_direction_at_10m` (degrees, `wind_from_direction`; **no u/v components**; whether direction is relative to true north on the LAEA grid is UNVERIFIED), `wind_gust_at_10m` (`wind_speed_of_gust`), `precipitation_rate` (`lwe_precipitation_rate`, **m s-1**, instantaneous), `rainfall_rate`, `snowfall_rate` (m s-1), `hail_fall_rate`, `cloud_amount_of_total_cloud` (`cloud_area_fraction`, 0-1), `snow_depth_water_equivalent`, `visibility_at_screen_level`, `fog_fraction_at_screen_level`. **No precip-type field and no simulated radar reflectivity.** Precip type must be derived from rain/snow/hail rates.
- Licence: **CC BY-SA 4.0**, "British Crown copyright 2023-2025 (as in the registry text), the Met Office". Share-alike: derived datasets or tile products may have to be released under CC BY-SA. Taking a legal view on tiles is out of scope here, but this is a real constraint compared with CC BY sources.
- Poll interval: the registry also publishes an SNS topic for new objects; not needed.

## 5. MET Norway (thredds.met.no)

Key-free. THREDDS catalog: `https://thredds.met.no/thredds/catalog/metno.xml` lists `mepslatest`, `meps25epsarchive`, `aromearcticlatest`, `aromearcticarchive` and others.

- **Access is OPeNDAP (DAP2) only for the "latest" products.** Plain HTTP `fileServer` returns **403** for the `.ncml` files; MET Norway documents (GitHub `metno/NWPdocs` wiki) that `latest` files are now NcML wrappers that "can no longer be downloaded directly but must be accessed via OPeNDAP". Subsetting is done by constraint expression or via netCDF4/xarray slices. Open took 0.9 s; one 2D field slice (1069 x 949 float32) took 0.3-1.2 s. NCSS is not enabled. UNVERIFIED: whether the older archive catalogs still allow `fileServer` (not tried).
- MEPS control member ("det"): `https://thredds.met.no/thredds/dodsC/mepslatest/meps_det_2_5km_20261106T15Z.ncml` (also `.das`, `.dds`, `.dods`). Runs every 3 h (00, 03, ..., 21) listed for the last 2 days; the 15Z run was published 17:23 UTC (+2 h 23). 67 hourly steps (0 to 66 h). Also `meps_lagged_6_h_*` ensemble products.
- MEPS grid: 949 x 1069, 2.5 km, `lambert_conformal_conic`, standard parallels 63.3 and 63.3, lon_0 15.0, lat_0 63.3, sphere R = 6,371,000 m, x -1,060,084 to 1,309,916 step 2500, y -1,332,518 to 1,337,482. 2D `latitude`/`longitude` arrays in the file. Corners (lat, lon): SW 50.32, 0.28; SE 49.77, 33.03; NW 72.76, -18.12; NE 71.58, 54.24.
- AROME-Arctic: `.../dodsC/aromearcticlatest/latest/arome_arctic_det_2_5km_20261106T15Z.ncml` (12Z and 15Z seen today), 739 x 949, 2.5 km Lambert conformal, standard parallels 77.5, lon_0 -25.0, lat_0 77.5, R 6,371,000. Corners (lat, lon): SW 69.30, -17.96; SE 62.13, 18.62; NW 87.57, 79.55; NE 71.11, 68.83 (Svalbard, Greenland Sea, Barents Sea, northern Norway). 67 time steps. Run cadence UNVERIFIED (the catalog shows 8 `det` files for roughly 2 days).
- Container: DAP (the underlying files are NetCDF; chunk sizes advertised `1,1,1069,949`, i.e. one 2D field per chunk). Float32, `_FillValue 9.96921e36`, no scale/offset. Projection variable `projection_lambert`.
- Variables (MEPS det): `air_temperature_2m` (K), `relative_humidity_2m` (fraction 0-1; no dew point, derive), `x_wind_10m`, `y_wind_10m` (**grid-relative**, along the projection x/y axes, rotate to east/north using the projection; `wind_speed`, `wind_direction` also present), `wind_speed_of_gust` (m/s), `precipitation_amount_acc` (kg m-2 accumulated since run start), `snowfall_amount_acc` ("total accumulated solid precipitation, snow+graupel+hail", kg m-2), `rainfall_amount` / `snowfall_amount` ("instantaneous ... at surface", kg m-2, interval not documented, UNVERIFIED), `precipitation_type` (float, values 0..5 observed at +3 h: counts 0:1637, 1:71630, 2:4365, 3:10267, 4:461, 5:1456; **no flag meanings in the file**, mapping UNVERIFIED, MET Norway convention probably 0 none, 1 rain, 2 sleet, 3 snow, 4 freezing rain, 5 freezing drizzle), `cloud_area_fraction` (total cloud, 0-1), `hail_diagnostic`, `lightning_index`. **No simulated radar reflectivity found.**
- Licence: NLOD and CC BY 4.0. Credit "The Norwegian Meteorological Institute" / "MET Norway" as source ("Data from The Norwegian Meteorological Institute", "Based on data from MET Norway"). MET Norway also asks for an identifying `User-Agent` and polite request rates (their terms of service page, not retrieved).

## 6. Quick pass over the others

| Source | Key needed | Verdict |
|---|---|---|
| **DMI HARMONIE (Denmark)** | No. Key-free S3 bucket and API (`https://opendataapi.dmi.dk/v1/forecastdata/collections`, bucket `dmi-opendata.s3.eu-north-1.amazonaws.com`). | **Usable and valuable**, section 7. |
| **FMI HARMONIE (Finland)** | No. `https://opendata.fmi.fi/download?producer=harmonie_scandinavia_surface&param=Temperature,WindUMS,WindVMS,WindGust,Precipitation1h,TotalCloudCover,DewPoint&format=grib2&starttime=...&endtime=...&bbox=19,59,32,70&projection=EPSG:4326` returned a GRIB2 (200, 887 KB for one param over 13 x 11 deg). Server-side subset and resample to regular lat-lon (0.0225 deg, 579 x 490 for that bbox), 5.0 simple packing. | Optional (Nordic overlap with MEPS/DINI). The model is the same family as MEPS. Parameter-to-field mapping partly UNVERIFIED (Precipitation1h returned `prate`). FMI licence CC BY 4.0 (UNVERIFIED, not retrieved). |
| MeteoSwiss ICON-CH1/CH2 | No account: STAC API `https://data.geo.admin.ch/api/stac/v1/collections/ch.meteoschweiz.ogd-forecasting-icon-ch1` hands out pre-signed `rgw.cscs.ch` URLs that expire in about 1 day. | ICON-CH1-EPS 1 km (33 h, every 3 h), ICON-CH2-EPS 2.1 km (120 h, every 6 h), control + perturbed members, one variable per file, last 24 h retained only. Domain only Switzerland/Alps (about 0.8W-17.7E, 42-50.5N), fully covered by ICON-D2/AROME. Grid and packing UNVERIFIED. **Skip.** CC BY. |
| GeoSphere Austria | No key. `https://dataset.api.hub.geosphere.at/v1/grid/forecast/nwp-v1-1h-2500m` (2.5 km, 1 h, geojson/netcdf subset API only). | `last_forecast_reftime` was **2026-11-04T03:00** (about 2.7 days stale at check). Subset-only API, no raw files. **Skip.** |
| KNMI HARMONIE | Yes: `api.dataplatform.knmi.nl` returns 401 "Authorization field missing". Sign-up: https://developer.dataplatform.knmi.nl/ . (An anonymous key is published in the docs and rotates, UNVERIFIED.) | Key-gated, skip. |
| AEMET | Yes (`opendata.aemet.es` needs an api_key): https://opendata.aemet.es/centrodedescargas/altaUsuario . Mostly text/JSON forecasts, not raw NWP grids. | Skip. |
| Belgium (RMI) | `opendata.meteo.be` responds without a key, but I found no NWP grids there. | No NWP found (UNVERIFIED). Skip. |
| Italy | ItaliaMeteo MeteoHub (`meteohub.mistralportal.it`) loads a UI; download apparently needs registration (UNVERIFIED). Arpae portal not checked. | Skip. |
| DWD ICON-EU / ICON-global | no | See sections 1, 8. |

Key-gated list for reference: KNMI https://developer.dataplatform.knmi.nl/ ; AEMET https://opendata.aemet.es/centrodedescargas/altaUsuario .

## 7. DMI HARMONIE (usable, details)

- API: `https://opendataapi.dmi.dk/v1/forecastdata/collections` (OGC API Features, JSON, no key). Collections of interest: `harmonie_dini_sf` (surface fields, Denmark-Iceland-Netherlands-Ireland, DINI), `harmonie_ig_sf` (Iceland-Greenland, IG), `_pl`/`_ml` pressure and model levels, `harmonie_dini_eps_*` (ensemble stats). Items give the S3 `href`.
- Direct S3: `https://dmi-opendata.s3.eu-north-1.amazonaws.com/forecastdata/HARMONIE_DINI_SF/HARMONIE_DINI_SF_<RUN>_<VALID>.grib` plus `...grib.index`. `<RUN>` and `<VALID>` are `2026-11-06T150000Z` style. Anonymous S3 listing works (`?list-type=2&prefix=forecastdata/HARMONIE_DINI_SF/HARMONIE_DINI_SF_2026-11-06T150000Z`). 200 today:
  `https://dmi-opendata.s3.eu-north-1.amazonaws.com/forecastdata/HARMONIE_DINI_SF/HARMONIE_DINI_SF_2026-11-06T150000Z_2026-11-06T180000Z.grib` (619.7 MB; `Accept-Ranges: bytes`).
- One file per valid hour containing all (93) surface fields. **0.5-0.62 GB per file**, so always fetch by range.
- `.index` is wgrib2 `-s`-style text: `n:byteoffset:d=YYYYMMDDHH:PARAM:level:range:ENS=hi-res ctl`, e.g. `7:39798982:d=2026110615:TMP:2 m above ground:3 hour fcst:ENS=hi-res ctl`. The length of a field = next offset minus this offset (last field: file size minus offset). Fields 6.1 MB (16-bit) or 9.2 MB (24-bit packing for accumulations). Verified by fetching six fields (about 48 MB total) and decoding them.
- Runs: 8 per day (every 3 h: 00, 03, ..., 21). DINI: 61 files (0-60 h hourly). IG: 73 files (0-72 h hourly). 15Z DINI: first file 16:36 (+1 h 36), last 17:43 (+2 h 43). IG 15Z: first 16:23, last 17:29. The 18Z run was not out at 18:30 UTC.
- DINI grid: template **3.30** Lambert conformal, Ni 1906, Nj 1606, 2000 m spacing, LaD = Latin1 = Latin2 = 55.5, LoV 352.0 (= -8), first point lat 39.671 lon 334.578 (=-25.42), scan: `jScansPositively=1` (south to north), `iScansNegatively=0`, earth shape 6 (sphere), `resolutionAndComponentFlags=56` => **winds are grid-relative** (rotate with the cone angle; flag bit "components relative to grid" is set; ICON/IFS/AROME use 48 = earth-relative). Bounding box from the API: lon -43.17 to 40.07, lat 37.69 to 69.91. Packing **5.0 simple**, 16 bits, no bitmap.
- IG grid: 3.30, 1339 x 1589, 2000 m, LaD = Latin1 = Latin2 = 72.0, LoV 324.0 (= -36), first point lat 55.776 lon 302.876 (=-57.12), flags 56, 5.0 simple. API bbox lon -109.8 to 37.8, lat 55.8 to 86.1 (Greenland, Iceland, Svalbard).
- Fields (shortName by eccodes, GRIB-style index name): `TMP:2 m above ground` (2t K); `DPT:2 m above ground` (dew point); `RH:2 m above ground` (%); `UGRD`/`VGRD:10 m above ground` (grid-relative, m/s); `GUST:10 m above ground:2-3 hour max fcst` (`max_i10fg`, max of the last hour, m/s; also `UGUST`/`VGUST`); **`TPRATE:surface:0-3 hour acc fcst`** is despite its name `tp`, accumulated kg m-2 since run start; `TSPRATE` = `titspf` total solid precipitation (snow+graupel) accumulated kg m-2; `RPRATE` rain, `GPRATE` graupel accumulations; `LCDC/MCDC/HCDC` cloud layers, `FRACCC`; and an unnamed parm (discipline 0, category 6, number 199, local table 1, values 0.01-1.0) that is probably total cloud cover (UNVERIFIED). `SDWE:0 m` is snow water equivalent (snow depth in water, kg m-2). `VIS`, `MIXHT`, `CAPE`, radiation. **No precip-type code and no simulated reflectivity** (no REFC/MAXREF in the 93-field index). Derive type from rain vs `titspf` accumulations (graupel is part of solid).
- Licence: CC BY 4.0. DMI terms of use page: credit the source, link the licence, indicate changes. "No additional restrictions" clause.

## 8. Which of these is worth what (cost and gotchas)

| Source | Value | Cost and gotchas |
|---|---|---|
| ECMWF IFS 0.1 deg | The global base | Needs 5.42. First column is 180 E; handle wrap. `tp` in metres, `tcc` fraction (but AIFS percent). 00z/12z 360 h, 06z/18z 144 h. Data is 6.5-9.5 h late; 0.1 comes 2 h later than 0.25 so fall back to 0.25 while waiting. |
| DMI DINI + IG | Largest high-resolution overlay for NW Europe and the N Atlantic, using the decoders we already have | Rotate grid-relative winds. Files are huge, so use `.index` ranges. `TPRATE` is actually accumulated amount. Precip type must be derived. |
| ICON-D2 | Very good overlay, decoders already exist | bz2 per file; bitmap with 9999 sentinel; 4 messages in some files (15-min); lowercase `2d_` naming. `ww` provides a ready ptype proxy. Domain smaller than DINI. |
| AROME France / OM | France, S Europe, tropics overseas | CCSDS only; no index (walk headers); 0.025 packs are 6-hour, parameter-grouped; step 0 absent for accumulations and cloud. Per-domain tiny files for OM. 3-4 h (France) and 6.5 h (OM) behind. |
| MEPS / AROME-Arctic | Fennoscandia and Arctic | OPeNDAP, Lambert, grid-relative winds, precip type table undocumented. |
| UKV / Met Office global | UK; global 10 km (better than ICON global but 5.5 h late, NetCDF only) | CC BY-SA. No ptype, no u/v. Rates in m s-1. |
| ICON-EU | Europe 6.5 km | Redundant if IFS 0.1 is the base. Needs 5.42. |
| ICON global | none | Icosahedral; not needed. |

## 9. Packing / grid templates we must support, by source

| Source | Grid | Packing |
|---|---|---|
| IFS / AIFS | 3.0 regular lat-lon, lon from 180 | **5.42** |
| ICON global | **3.101** unstructured (+CLAT/CLON files) | **5.42** |
| ICON-EU | 3.0 | **5.42** |
| ICON-D2 | 3.0 (+ bitmap) | 5.0 |
| AROME (all) / ARPEGE | 3.0 (+ bitmap) | **5.42** |
| DMI DINI / IG | 3.30 Lambert (+ grid-relative winds) | 5.0 |
| FMI (download API) | 3.0 (resampled) | 5.0 |
| Met Office | NetCDF4 (HDF5, zlib) | n/a |
| MET Norway | NetCDF over DAP2 | n/a |

New work compared with the current code: 5.42 decoder, template 3.101 handling (or avoid by not using ICON global), bitmap-aware 3.0 for D2/AROME (the grid crate already reads section 6 for D2-type bitmaps, status UNVERIFIED), grid-relative wind rotation for 3.30 (flag bit), NetCDF/DAP readers only if MEPS or UKV are adopted.

## 10. Items marked UNVERIFIED (consolidated)

- Whether the `grib` crate handles template 5.42 (CCSDS) and bitmaps for 3.0 with 5.0.
- IFS 12z release time and 0.1 deg 06z/18z full horizon (06z checked only to 90 h for 0.1; 06z 0.25 list ends 144 h and the 18z 150 h file is 404).
- ptype code names for ECMWF (WMO table 4.201) and for MET Norway (`precipitation_type` 0-5).
- AROME 0.01 reflectivity units; AROME OM INDIEN run frequency; ARPEGE parameter list.
- Met Office wind-direction reference frame on the LAEA grid; 15-minute file horizon.
- DMI total-cloud parameter (cat 6 num 199) meaning; FMI parameter-to-field mapping; FMI licence wording.
- Attribution wording for ECMWF and DWD (DWD publishes "Rules and templates for acknowledging the DWD as source"; I did not retrieve the template text). DWD licence: open spatial data and high-value datasets under CC BY 4.0 with the source acknowledged (their legal notice page); the GeoNutzV phrasing common in older documentation was not retrieved from the page.
- Rate limits: none found on any host; no documented limit for opendata.dwd.de beyond the access terms (IP kept 7 days).

## 11. Sample files saved

Base: `/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/europe-models/` (about 264 MB in total).

- `dwd/`: `icon_global_icosahedral_single-level_2026110612_003_T_2M.grib2`, `..._006_TOT_PREC.grib2`, `..._006_VMAX_10M.grib2`, `icon_global_icosahedral_time-invariant_2026110612_CLAT.grib2`, `..._CLON.grib2`, `icon-d2_germany_icosahedral_time-invariant_2026110612_000_0_clat.grib2`, `icon-d2_germany_regular-lat-lon_single-level_2026110612_003_2d_t_2m.grib2`, `..._003_2d_dbz_cmax.grib2` (4 msgs), `..._003_2d_prs_gsp.grib2` (4 msgs), `..._006_2d_clct.grib2`, `..._006_2d_snow_gsp.grib2`, `..._006_2d_tot_prec.grib2`, `..._006_2d_vmax_10m.grib2`, `..._006_2d_ww.grib2`, and `icon-eu_europe_regular-lat-lon_single-level_2026110612_003_{T_2M,TOT_PREC,U_10M,WW}.grib2`, `..._006_{CLCT,H_SNOW,SNOW_GSP,TD_2M,VMAX_10M}.grib2` (all decompressed).
- `ecmwf/`: `ifs0p1_00z_3h_subset.grib2` (12 messages: 2t 2d 10u 10v 10fg tp tprate ptype tcc sd sf rsn), `ifs0p25_00z_3h_subset.grib2` (2t tp ptype), `aifs0p25_00z_6h_subset.grib2` (2t tp 2d tcc).
- `meteofrance/`: `arome__001__SP1__03H__2026-11-06T12:00:00Z.grib2`, `..._SP2__03H...`, `..._SP3__03H...`, `arome0025_SP1_00H06H_head.grib2` (first 24 whole messages of the 0.025 SP1 6-hour pack), `arome0025_SP2_00H06H_head.grib2`, `arome0025_SP3_00H06H_head.grib2`, `arome-om_POLYN_SP1_001H.grib2`, `arpege025_SP1_head.grib2`, `arome0025_header_walk.txt` (message offset map of the 0.025 packs), `arpege025_SP1_walk.txt`.
- `metoffice/`: `uk_temperature_at_screen_level.nc`, `uk_temperature_of_dew_point_at_screen_level.nc`, `uk_wind_speed_at_10m.nc`, `uk_wind_direction_at_10m.nc`, `uk_wind_gust_at_10m.nc`, `uk_precipitation_rate.nc`, `uk_snowfall_rate.nc`, `uk_cloud_amount_of_total_cloud.nc`, `global_temperature_at_screen_level.nc`, `global_precipitation_rate.nc`, `global_wind_gust_at_10m.nc`.
- `metno/`: `meps_det_20261106T15Z_t03_subset.nc` (nine MEPS fields at lead 3 h, re-saved locally).
- `dmi/`: `dini_15Z_3h_subset.grib` (t2m, u10, gust, tp, solid precip, cloud), `ig_15Z_3h_t2m.grib`, `dini_index.txt`, `ig_index.txt`.
- `fmi/`: `fmi_harm.grib`, `fmi_multi.grib`.
- `docs/`: Météo-France PDFs (`descriptiontechnique-paquetsarome-donneespubliques-v4-20250401.pdf`, `description-parametres-modeles-arpege-arome-v2-185.pdf`).
- Helper scripts (not samples): `scripts/gdump.py`, `msgs.py`, `list.py`, `rangewalk.py`, `hdrwalk.py`, `sec3.py`, `ecfetch.py` in the scratchpad parent folder.
