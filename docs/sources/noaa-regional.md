# NOAA sources beyond the lower 48 (verified 2026-11-06)

All facts below were fetched on 2026-11-06 between 18:10 and 18:40 UTC from anonymous
HTTPS S3 endpoints (`https://<bucket>.s3.amazonaws.com/...`), with `User-Agent: weather-map-dev`,
<= 2 requests/s, prefix + delimiter listings only. Grid parameters were decoded two ways: a
small section walker (octets per the GRIB2 spec) and eccodes 2.50 (pip wheel in a scratch venv);
they agree. Anything not verified is marked UNVERIFIED. Total downloaded: about 40 MB.

Samples live in `/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/noaa/`
(subfolders `mrms rtma hrrrak nbm gfs nam rrfs`, helper scripts in `tools/`).

## 0. Summary and what the decoder must grow

| Source | Usable | Grid template | Packing seen | Gotcha |
|---|---|---|---|---|
| MRMS ALASKA / HAWAII / CARIB / GUAM | yes, drop-in | 3.0 lat-lon (0.01 or 0.005 deg) | 5.41 PNG | none beyond per-domain grid; lons are 0..360 |
| RTMA AK | yes | 3.20 polar stereographic | 5.0 | new grid template; winds grid-relative |
| RTMA HI / PR / GU | yes | 3.10 Mercator | 5.0 | new grid template; Guam dir is `gurtma` |
| HRRR Alaska | yes | 3.20 polar stereographic | 5.0 and 5.3 mixed | 90-105 MB files, idx ranges essential; winds grid-relative |
| NBM ak / hi / pr / gu / oc (and co) | yes (except oc) | ak 3.20; hi/pr/gu 3.10; co 3.30 | 5.3 (one Guam record 5.2) | scan mode 0x50 (alternating rows); no u/v; no precip type outside ak/co |
| NAM nests (Alaska, Hawaii, firewx) | no (stale since 2026-11-03) | 3.10 / others | 5.40 JPEG2000 | no new data for 3 days; JPEG2000 unsupported; no PR nest |
| HIRESW | not found | | | tried `noaa-hiresw-pds`, `noaa-nws-hiresw-pds`: NoSuchBucket |
| RRFS (`noaa-rrfs-pds`) | no | 3.1 rotated lat-lon (sample) | 5.3 | only retro/sample data, no live feed |
| URMA AK/HI/PR | marginal | same as RTMA | | about 6 h latency; no Guam |
| GFS 0.25 | yes (already) | 3.0 | 5.3 | two APCP records per file; no ASNOW |

New decoding work required for the regional mosaic:

1. Grid template 3.20 (polar stereographic): RTMA AK, HRRR AK, NBM AK.
2. Grid template 3.10 (Mercator): RTMA HI/PR/GU, NBM HI/PR/GU. For NBM the "orientation of the grid"
   octets are non-zero (HI 200, PR 295, GU 0) but the geometry is plain Mercator with no rotation
   (checked: HI longitude span 14.93 deg vs 624 x 2.5 km x cos(20 deg) agrees). Treat them as noise.
3. Scanning mode 0x50 (bits: +j and alternating row direction) in every NBM file; eccodes reports
   `alternativeRowScanning=1`. Odd rows must be reversed. All other sources here use mode 0 (MRMS, GFS: north to
   south) or 64 (RTMA, HRRR: south to north, same direction every row).
4. Wind rotation: resolution-and-component flags bit `0x08` ("u/v relative to grid") is SET for RTMA
   (all regions), HRRR AK, NAM nests, RRFS; it is CLEAR for NBM and GFS and MRMS. On polar stereographic
   grids (RTMA AK, HRRR AK) rotate by (lon - LoV) for a north-polar grid; on Mercator grids no rotation is needed
   (grid axes are east/north). (The rotation formula is standard theory, not measured here.)

## 1. MRMS regional domains (`noaa-mrms-pds`)

What: same Multi-Radar Multi-Sensor system as CONUS, one prefix per domain. Prefixes at bucket root:
`ALASKA/ ANC/ CARIB/ CONUS/ CONUS_5KM/ ConvectProb/ GUAM/ HAWAII/ ProbSevere/ unsupported/`.

Products present in all four domains (each ~150 product folders; listing of `ALASKA/` etc.): every product
we use in `layer.rs` exists with identical names:
`MergedReflectivityQCComposite_00.50`, `RadarOnly_QPE_01H_00.00`, `RadarOnly_QPE_24H_00.00`,
`RadarOnly_QPE_72H_00.00`, also `PrecipRate_00.00`, `PrecipFlag_00.00`, `RadarOnly_QPE_{15M,03H,06H,12H,48H,Since12Z}_00.00`,
`MultiSensor_QPE_*_Pass1/Pass2`, `SeamlessHSR_00.00`, `EchoTop_*`, `VIL_*`, `MESH_*`. (Hawaii/Carib/Guam also have
AzShear and RotationTrack; Alaska does not.)

Key pattern (one concrete example that returned 200 today):
`HAWAII/RadarOnly_QPE_01H_00.00/20261106/MRMS_RadarOnly_QPE_01H_00.00_20261106-181400.grib2.gz`
i.e. `<DOMAIN>/<Product>/<YYYYMMDD>/MRMS_<Product>_<YYYYMMDD>-<HHMMSS>.grib2.gz`. No `.idx` (single message per file, gzip).
Find newest: list `<DOMAIN>/<Product>/` with delimiter to get date folders (listing is ascending and truncates at 1000 keys:
use `start-after=<DOMAIN>/<Product>/<YYYYMM>00/`), then list the day folder and take the last key. Per-field ranges are N/A
(one field per file).

Cadence and latency (measured at 18:17-18:18 UTC; LastModified minus valid time):

| Product | Cadence | Latency |
|---|---|---|
| MergedReflectivityQCComposite | 2 min (HH:MM:SS drifts, e.g. 181255, 181455 in AK/HI; 181456, 181656 in CARIB/GUAM) | 1-2 min |
| RadarOnly_QPE_01H, PrecipRate | 2 min on even minutes (181200, 181400), plus 24H/72H on the hour | 2-3 min |
| RadarOnly_QPE_24H / 72H | hourly, on the hour (170000, 180000) | 2-3 min after the hour |

Same as CONUS (checked side by side).

Grids (all template 3.0, earth shape 2 = IAU 1965 spheroid, res flags 0x30, scan mode 0 = +i, -j, i.e. first point is
north-west; first-point longitudes are 0..360):

| Domain | nx x ny | dx = dy | first point (NW) lat, lon | last point (SE) lat, lon | bbox (lon -180..180) |
|---|---|---|---|---|---|
| ALASKA | 5000 x 2200 | 0.01 | 71.995, 184.005 | 50.005, 233.995 | 50-72 N, 175.995 W to 126.005 W |
| HAWAII | 2600 x 2200 | 0.005 | 25.998, 196.002 (composite: 25.9975, 196.0025) | 15.003, 208.997 | 15-26 N, 164 W to 151 W |
| CARIB | 3000 x 1500 | 0.01 | 24.995, 270.005 | 10.005, 299.995 | 10-25 N, 89.995 W to 60.005 W |
| GUAM | 2000 x 1800 | 0.005 | 17.998, 140.003 (composite 17.9975, 140.0025) | 9.003, 149.998 | 9-18 N, 140 E to 150 E |

Gotcha: the QPE/PrecipRate grids and the reflectivity grids differ by 0.0005 deg (half a pixel at 0.005 deg is 0.0025, so
this is sub-pixel) in HAWAII and GUAM; the reflectivity files are offset by pixel centre. Alaska's domain stops at 176 W, so the western
Aleutians and Kamchatka side are not covered; it overlaps the CONUS domain over 50-55 N, 126-130 W. The Alaska
domain is mostly ocean/no-coverage (63% of composite cells are the -999 sentinel).

Message identity: discipline 209 (local), Alaska composite pcat 10 pnum 0 (level type 102 = mean sea level... value 500 for the 0.50
suffix), QPE 01H pcat 6 pnum 2, QPE 24H pcat 6 pnum 6. Packing 5.41 (PNG) in every file. Decoded values: reflectivity
-999 = no coverage, -99 = no echo (same as CONUS); QPE/rate -3 = no coverage, 0 = none; QPE is mm (accumulation ending at the
valid time). `missingValue` key is 9999, never actually used.

Sizes (gzip): composite AK 213 KB, HI 54 KB, CARIB 104 KB, GUAM 76 KB (CONUS 1.28 MB); QPE 1H 56-91 KB;
24H 126-455 KB; 72H 427-701 KB (CONUS 7.2 MB). Decompressed single message about 60-110 KB for 1H products (PNG) because the
grid is mostly missing.

Samples: `mrms/{ALASKA,HAWAII,CARIB,GUAM}.{refc,qpe1h}.grib2.gz`, `mrms/HAWAII.{qpe24h,rate}.grib2.gz`, decompressed copies in `mrms/raw/`.

History depth was not walked (not needed for live). Note the first 1000-key listing page ends at 2023-07-10 for most
Alaska/Hawaii/Carib/Guam products, but at 2025-04 for Guam QPE/PrecipRate, so Guam QPE keys are sparser or start later (UNVERIFIED).

## 2. RTMA (`noaa-rtma-pds`) and URMA (`noaa-urma-pds`)

Directory prefixes (verified newest day exists for each): `akrtma.YYYYMMDD/` (Alaska), `hirtma.` (Hawaii), `prrtma.`
(Puerto Rico + USVI), `gurtma.` (Guam; note NOT `guamrtma`), `rtma2p5.` (CONUS, 2.5 km, files end `.grb2_wexp`),
`rtma2p5_ru.` (CONUS rapid update, 15 min). URMA: `akurma. hiurma. prurma. urma2p5.`; no Guam URMA prefix found.

File names (hour `t17z`; three kinds each hour):
- analysis `akrtma.20261106/akrtma.t17z.2dvaranl_ndfd_3p0.grb2` (AK has `_3p0`); `hirtma.20261106/hirtma.t17z.2dvaranl_ndfd.grb2`;
  same pattern `prrtma`, `gurtma` (each returned 200 today, each has `.idx`).
- first guess `...2dvarges_ndfd...` and error `...2dvarerr_ndfd...` (not needed).

Cadence and latency: hourly. t17z analysis objects were written 17:34-17:39 (35-40 min after the valid hour);
at 18:30 t18z did not exist yet. (CONUS rtma2p5 t17z at 17:50; rtma2p5_ru has 15-min cycles, 18 min latency, CONUS only. No rapid-update for AK/HI/PR/GU.)
URMA t11z was written at 17:43 (6.7 h latency): useless for live.

Fields (identical `.idx` line pattern; short, single group of ~13 records; the format is `n:offset:d=YYYYMMDDHH:VAR:level:anl:`):
`HGT:surface`, `PRES:surface`, `TMP:2 m above ground`, `DPT:2 m above ground`, `UGRD:10 m above ground`,
`VGRD:10 m above ground`, `SPFH:2 m above ground`, `WDIR:10 m above ground`, `WIND:10 m above ground`,
`GUST:10 m above ground` (10 m, not "surface"; matches `GUST_10M` in layer.rs), `VIS:surface`, `CEIL:cloud ceiling`,
`TCDC:entire atmosphere (considered as a single layer)` (this is the string; layer.rs uses `entire atmosphere` for HRRR/GFS, so match by prefix),
Guam additionally `HTSGW:surface` (wave height). No precipitation fields at all.
Units: K, m/s, degrees, %, m, Pa. All 'anl' (instantaneous).

Grids (all earth shape 1 = sphere R 6367470 m, scan mode 64, res flags: HI/PR/GU 0x38, AK 0x08, so winds ARE grid-relative; packing 5.0 simple, 12-16 bits):

| Region | Template | nx x ny | dx | First point (SW) | Other | Coverage |
|---|---|---|---|---|---|---|
| AK | 3.20 polar stereographic | 1649 x 1105 | 2976.563 m | 40.5301 N, 181.429 E (178.571 W) | LaD 60 N, LoV 210 E (150 W), north pole | lat 40.5-75.4, lon wraps 150 E ... 180 ... 93.6 W (huge: Aleutians, Bering, Yukon, N Pacific) |
| HI | 3.10 Mercator | 321 x 225 | 2500 m | 18.0727 N, 198.475 E | LaD 20 N; last point 23.0878 N, 206.131 E; orientation 0 | 18.07-23.09 N, 161.525 W-153.869 W |
| PR | 3.10 Mercator | 353 x 257 | 1250 m | 16.8287 N, 291.8047 E | last 19.7362 N, 296.0155 E | 16.83-19.74 N, 68.195 W-63.985 W |
| GU | 3.10 Mercator | 193 x 193 | 2500 m | 12.3499 N, 143.6865 E | last 16.7944 N, 148.28 E | 12.35-16.79 N, 143.69-148.28 E |

Sizes: AK file 41,000,503 B (about 3 MB per field); HI 1.59 MB (about 108 KB per field); PR 1.90 MB; GU 0.79 MB.
Per-field ranges: yes, via the `.idx` byte offsets (`n:offset:...`; end of a record is next offset - 1; last
record runs to EOF).

Samples: `rtma/{hirtma,prrtma,gurtma}.t17z.anl.grb2` + `.idx`, `rtma/akrtma.t17z.anl.idx`, `rtma/akrtma.head.bin` (first 3 KB of the AK file).

Surprising: AK RTMA is the same 3 km polar grid as NBM AK (same nx, ny, LoV), so one AK projection serves both.

## 3. HRRR Alaska (`noaa-hrrr-bdp-pds`, prefix `hrrr.YYYYMMDD/alaska/`)

What: HRRR-Alaska, 3 km, still operational (CONUS HRRR also still running: `hrrr.20261106/conus/` t17z f00 written 17:54).
Day folder lists `alaska/` and `conus/`.

Cycles: every 3 h (00, 03, 06, 09, 12, 15, 18, 21 UTC). Forecast length: 18 h for the off-synoptic cycles (03, 09, 15, 21), 48 h for 00/06/12/18 (verified: t00z/t06z/t12z have f00..f48; t03z/t09z/t15z f00..f18).

Products per cycle (hourly files except subh): `hrrr.t12z.wrfsfcf01.ak.grib2` (surface, ~90-106 MB each), `wrfprsf` (pressure levels, ~250-285 MB),
`wrfnatf` (native, ~450-490 MB), `wrfsubhf00..18` (sub-hourly, 4 time steps per file, 31-165 MB), plus `bufrsnd.tar.gz`, `class1.bufr`.
All have `.idx` (wrfsfc: 169 lines; wrfsubh: 196 lines). Example 200 today:
`https://noaa-hrrr-bdp-pds.s3.amazonaws.com/hrrr.20261106/alaska/hrrr.t12z.wrfsfcf01.ak.grib2` (+`.idx`).

Latency (LastModified of f00/last file): t12z f00 12:53 (53 min), f48 13:24 (84 min); t15z f00 15:46 (46 min), f18 16:19 (79 min);
t00z subh f00 at 00:43. At 18:30 t18z was not out and t15z was the newest complete cycle (t16z/t17z/t18z do not exist for AK).
Find newest: list `hrrr.<today>/alaska/hrrr.t` prefixed by cycle hour (list `hrrr.t15z.wrfsfcf` etc., a full-day listing exceeds 1000 keys).

Grid (all products): template 3.20 polar stereographic, 1299 x 919, 3000 m, first point 41.612949 N, 185.117126 E (174.883 W),
LaD 60, LoV 225 (135 W), north pole, scan 64, earth shape 6 (sphere R 6371229 m), res flags 0x08 (winds grid-relative). Coverage about lat 41.6-77.1,
lon from 156 E across the dateline to 116 W.

Fields in wrfsfc idx (strings as in the file; all hourly steps are `N hour fcst`):
`REFC:entire atmosphere`, `GUST:surface`, `TMP:2 m above ground`, `DPT:2 m above ground`, `UGRD:10 m above ground`, `VGRD:10 m above ground`,
`PRATE:surface`, `CRAIN/CSNOW/CICEP/CFRZR:surface`, `ASNOW:surface:0-N hour acc fcst` (run total; at f00 `0-0 day acc fcst`, at f48 `0-2 day acc fcst`: unit is
days from 24 h on), `APCP:surface:0-N hour acc fcst` and a second `APCP:surface:(N-1)-N hour acc fcst`, `TCDC:entire atmosphere`,
`WEASD`, `SNOD`, `RH:2 m above ground`, `VIS:surface`. Same names as the CONUS HRRR registry; no layer.rs change needed apart from grid.
Subhourly files: `REFC:entire atmosphere:15 min fcst` then 30/45/60 min in the same file; f01 holds 15/30/45/60; precip type flags and PRATE are also in there.

Packing seen: mixed; TMP 2 m, REFC, PRATE are 5.3 complex+spatial diff; UGRD 10 m is 5.0 simple (10 bits). Per-field sizes (f01): TMP 2 m 688 KB, UGRD 1.49 MB, REFC 671 KB, PRATE 255 KB.
Byte ranges work (verified with 4 Range requests).

Samples: `hrrrak/sfc.f01.{TMP2m,UGRD10m,REFC,PRATE}.grib2`, `hrrrak/{sfc.f01,subh.f01,sfc.f00,sfc.f02,sfc.f06,sfc.f18,sfc.f48}.idx`.

## 4. NBM (`noaa-nbm-grib2-pds`)

Layout: `blend.YYYYMMDD/HH/core/blend.tHHz.core.fFFF.<domain>.grib2` (+`.idx`), domains `ak co gu hi oc pr` (6 files per forecast hour),
and `blend.YYYYMMDD/HH/text/` (not examined). Example 200 today:
`blend.20261106/17/core/blend.t17z.core.f001.pr.grib2` (+ `.idx`). Hourly cycles 00..17 present for today (t18z not out at 18:30).
(`noaa-nbm-pds` is a second, older-version bucket: `blendv3.2 ... blendv5.0`; not used.)

Latency (LastModified): hourly cycle t17z f001 written 17:42-17:47 (about 45 min after the cycle label; all hours of the cycle within 5 min);
t12z and t00z (long) cycles: 13:06 and 01:03 (63-66 min). Whole cycle appears within a few minutes.

Horizon and step: hourly cycles (e.g. 17z): f001-f049 hourly, then 3-hourly to f193, then 6-hourly to f259. Synoptic 00/12 (and expected 06/18): hourly to f048,
3-hourly to f192, 6-hourly to f264 (t12z last f264; t17z f262 -> 404). There is no f000. Step in the table = N hour fcst.

Grids (all scan mode 0x50 = +j and alternating rows):

| Domain | Template | nx x ny | dx | First point (SW) | Other | Notes |
|---|---|---|---|---|---|---|
| ak | 3.20 polar stereo | 1649 x 1105 | 2976.56 m | 40.53 N, 181.429 E | LaD 60, LoV 210, earth 1 | identical grid to RTMA AK |
| co | 3.30 Lambert | 2345 x 1597 | 2539.703 m | 19.229 N, 233.7234 E | LaD 25, LoV 265, Latin1=Latin2=25, earth 1 | |
| hi | 3.10 Mercator | 625 x 561 | 2500 m | 14.3515 N, 195.0305 E | last 26.8605 N, 209.9598 E | 14.4-26.9 N, 165-150 W |
| pr | 3.10 Mercator | 339 x 225 | 1250 m | 16.9775 N, 291.9722 E | last 19.5221 N, 296.0156 E | |
| gu | 3.10 Mercator | 193 x 193 | 2500 m | 12.3499 N, 143.6865 E | last 16.7944 N, 148.28 E | same grid as RTMA GU |
| oc | not examined | | | | | only 11 records (TSTM, PWAT probabilities, THUNC); no surface weather. Not useful |

Res flags 0x30 (ak, co) or 0x00 (hi, pr, gu); either way u/v relative flag is clear (not relevant since NBM has no u/v).
Earth shape 1 (R 6367470 m). Packing: 5.3 for essentially every record (Guam has one 5.2 record).

Fields (exact `.idx` strings, f001; windows are noted):
- `TMP:2 m above ground:1 hour fcst`, `DPT:2 m above ground:1 hour fcst`, `APTMP:2 m above ground:1 hour fcst` (apparent temperature; missing for gu), `RH:2 m above ground`.
- Wind: `WIND:10 m above ground`, `WDIR:10 m above ground`, `GUST:10 m above ground`. There is NO UGRD/VGRD in NBM; build u/v from speed and meteorological direction (from-direction).
  Also WIND/WDIR at 30 m, 80 m, and `surface - 610 m` layers.
- Sky: `TCDC:surface:1 hour fcst` (total cloud cover; ak, co, hi, pr, gu). `TCDC:high cloud layer` in co only.
- Precip: `APCP:surface:<window> hour acc fcst`: f001 shows `0-1`; then 1-hour windows (`5-6`, `11-12`, `47-48`) through f048, f049 is a 6-hour window `43-49`, then only every 6 h with
  6-hour windows (`49-55` at f055, `55-61`, ... `253-259`); f052, f058 etc. have NO APCP. No PRATE.
- Snow (ak and co only): `ASNOW:surface:<window> hour acc fcst` (same windows), `SNOD:surface`, `SNOWLR:surface`, `SNOWLVL:0 m above mean sea level`. No snow fields in hi, pr, gu.
- Precip type: only probabilities (`PTYPE:surface:1 hour fcst:prob >=1 <2:prob fcst 1/1` for categories 1-2, `>=3 <4`, `>=5 <7`, `>=8 <9`; plus `PWTHER`, `FRZSPR`, `FICEAC` in ak/co). No CRAIN/CSNOW/CFRZR/CICEP flags and no PTYPE in hi/pr/gu.
- Also `VIS`, `CEIL`, `MAXREF:1000 m above ground`, `PWAT`, `CAPE`, `DSWRF`, `RETOP`, `VIL`, `TSTM` probability, plus `... ens std dev`, `prob >x`, `10% level` percentile records (about 40% of the file).
- No composite reflectivity (REFC) in NBM; only MAXREF at 1000 m AGL.

Units: K, %, m/s, degrees, kg m-2 (mm) for APCP, m for ASNOW/SNOD (ASNOW 0.00254 m = 0.1 in thresholds in probabilities), m for VIS/CEIL.
Per-field ranges: yes via idx. Because the files carry the whole ensemble statistics, files are large: ak 73 MB, co 165 MB, hi 8.7 MB, pr 1.5 MB, gu 0.6 MB, oc 11 MB (f001).
Per-field (f001): TMP 2 m ak 949 KB, co 2.07 MB, hi 89 KB, pr 26 KB, gu 11 KB; APCP ak 783 KB, hi 136 KB, pr 58 KB; WIND ak 972 KB, hi 140 KB; GUST ak 840 KB, hi 133 KB; TCDC ak 683 KB, hi 120 KB; ASNOW ak 320 KB.

Samples: `nbm/f001.{hi,pr,gu}.grib2` (+ `.idx` for all six domains `nbm/f001.<d>.idx`), `nbm/f001.{ak,co}.TMP2m.grib2`, `nbm/pr.f*.idx`.

## 5. Other NOAA high-res output on AWS

- NAM (`noaa-nam-pds`): files named `nam.YYYYMMDD/nam.tHHz.<product>...`. Nests exist: `alaskanest.hiresfNN.tm00.grib2` (about 812-836 MB per file), `hawaiinest.hiresfNN.tm00.grib2` (about 19 MB),
  `conusnest`, `firewxnest`; NO `prinest` found. The newest day in the bucket is 2026-11-03 (only t00z and t06z for Hawaii; no t12z/t18z; 20261104-06 empty), i.e. stale by three days
  (UNVERIFIED whether temporary). Hawaii nest grid: template 3.10 Mercator, 321 x 225, 2.5 km (same grid as `hirtma`), first point 18.073 N, 198.475 E, winds grid-relative;
  packing 5.40 (JPEG2000), which the `grib` crate path we use does not decode; `.idx` is present (records like `TMP:2 m above ground:anl`, `PRATE:surface`). Verdict: do not adopt.
  Sample: `nam/hawaiinest.f00.idx`.
- HIRESW: no public bucket under the names tried (NoSuchBucket). UNVERIFIED whether it is published elsewhere.
- Other bucket probes: `noaa-rap-pds` (RAP, CONUS-only domain, not examined), `noaa-gefs-pds` (ensemble, not examined).

## 6. RRFS (`noaa-rrfs-pds`)

Bucket root holds only `retro_output_final/{spring,summer,winter}/`, `rrfs_retro_maps/` (dated 2023-07 onward, maps only) and `sample_rotlat_awips/`
(50 objects, uploaded 2026-08-28: `grib2.rrfs.t00z.13km.fNNN.na`). There is no daily-dated live feed. The sample is 13 km (not 3 km), grid template 3.1 (rotated lat-lon), 1127 x 683, Dx = Dy = 108300
(units in the rotated frame), packing 5.3, res flags 0x38 (grid-relative winds). Conclusion: no operational or real-time RRFS data on this bucket; HRRR (conus and alaska) is
still running and being published every cycle. UNVERIFIED whether RRFS is published at another bucket/name (`noaa-nws-rrfs-pds` is NoSuchBucket).
Samples: `rrfs/sample.head.bin`, `rrfs/sample.f000.part` (12 MB range).

## 7. GFS (`noaa-gfs-bdp-pds`)

Key: `gfs.YYYYMMDD/HH/atmos/gfs.tHHz.pgrb2.0p25.fFFF` (+ `.idx`), HH in 00/06/12/18. Example 200 today:
`gfs.20261106/12/atmos/gfs.t12z.pgrb2.0p25.f006` and `.f006.idx`. Siblings: `pgrb2b.0p25` (349 records, no ASNOW), `sfluxgrb2` (no ASNOW). Wave in `.../wave/`.

Steps: f000-f120 hourly (121), then 3-hourly to f384 (88 more) = 209 files per run (counted for the 06z and 12z runs).
Latency (LastModified): 12z f000 15:33 (3 h 33 min after the cycle), f001 15:33, last f384 17:16 (5 h 16 min). 06z: f001 09:31, f384 11:12 (5 h 12 min). So a full run is available about 5 h 15 min after the cycle; the first 120 h hourly set is
available roughly 3.5-4.5 h after.

Grid: template 3.0 lat-lon, 1440 x 721, Dx = Dy = 0.25 deg, first point 90 N, 0 E; last -90 S, 359.75 E, scan mode 0, earth shape 6, res flags 0x30 (not grid-relative), packing 5.3 for every record checked.

Exact `.idx` records (12z, strings after the date field):

| Quantity | Record | Notes |
|---|---|---|
| 2 m temp | `TMP:2 m above ground:N hour fcst` (f000: `anl`) | K |
| 2 m dew point | `DPT:2 m above ground` | K |
| 10 m wind | `UGRD:10 m above ground`, `VGRD:10 m above ground` | m/s, not grid-relative |
| Gust | `GUST:surface` | m/s; at f000 `anl`; min value positive |
| Precip rate | `PRATE:surface:N hour fcst` instantaneous, and `PRATE:surface:0-N hour ave fcst` (mean since reset window) | kg m-2 s-1 |
| Accumulated precip | two `APCP:surface` records: first = window since last 6 h reset, second = run total `0-N hour acc fcst` | see below |
| Precip type | `CRAIN:surface`, `CSNOW:surface`, `CFRZR:surface`, `CICEP:surface`, each as instantaneous `N hour fcst` and as `0-N hour ave fcst` | categorical 0/1 for instantaneous (bits 3 per value) |
| Total cloud | `TCDC:entire atmosphere:N hour fcst` (also `0-N hour ave fcst`) | %. Beware `TCDC:50 mb` ... `TCDC:1000 mb` also exist (isobaric); also `LCDC:low cloud layer`, `MCDC:middle cloud layer`, `HCDC:high cloud layer` |
| Snow | `SNOD:surface`, `WEASD:surface` (both instantaneous). There is NO `ASNOW` in pgrb2, pgrb2b or sflux. `CSNOW` and `CPOFP:surface` also exist | derive snowfall from APCP x CSNOW or difference of WEASD/SNOD |
| Composite reflectivity | `REFC:entire atmosphere:N hour fcst` | dBZ; no echo = -20 dBZ; fully populated at all hours |
| Other | `APTMP:2 m above ground`, `RH:2 m above ground`, `PWAT:entire atmosphere (considered as a single layer)`, `ICEC:surface` | |

APCP windows (record 596 first, 597 second): f001-f006 both records are `0-N hour acc fcst` (identical). f009: `6-9` and `0-9`; f012: `6-12` and `0-12`; f123: `120-123` and `0-123`.
So records 596/597 are the same record number at all steps in this idx but the first one is not the run total after f006; pick the record whose window starts at `0-`. In any case the match must be on
the window string, not on "APCP:surface" alone (two matches).

Gotcha: `RH:2 mb` (pressure level) shares the prefix `RH:2 m` with `RH:2 m above ground`; the same for `UGRD:10 mb` vs `UGRD:10 m above ground`. A substring match on `2 m` or `10 m` is wrong; match the full level string.

Sizes (f006): whole file ~500-550 MB (f000 501.7 MB, f384 549 MB). Per field: TMP 2 m 511 KB, APCP 365 KB, CRAIN 104 KB, REFC 875 KB, UGRD 10 m 966 KB, GUST 613 KB, TCDC 863 KB, PRATE 674 KB. Idx 743 records at f003+, 696 at f000.
Samples: `gfs/f006.{TMP2m,APCP,CRAIN,REFC,UGRD,GUST,TCDC,PRATE}.grib2` and idx files `gfs/f000.idx f001 f002 f003 f004 f005 f006 f009 f012 f123 f384`.

## 8. Recommendation (ranked)

1. MRMS AK/HI/CARIB/GUAM: identical product names, template 3.0 + 5.41, 2-minute radar and QPE. Only a per-domain grid table is needed. Highest value, no decoder work.
2. GFS as the global base (already ingested): add `APCP` window disambiguation and derive snow (no ASNOW).
3. Mercator (3.10) + polar stereographic (3.20) + alternating-row scan support in the decoder. This one investment unlocks items 4-6.
4. NBM hi/pr/gu/ak for forecast temperature, dew point, apparent temperature, wind/gust (speed+direction only), cloud, precipitation (hourly to f48): the only sharp public forecast for PR/Guam/Hawaii. Use per-field ranges.
5. HRRR Alaska for 3 km AK forecast + sub-hourly simulated radar (REFC) and ptype flags. Heavy files, ranges required, 3-hourly cycles, 18 h or 48 h.
6. RTMA AK/HI/PR/GU analyses (hourly, about 35-40 min late) for "now" temperature/wind/cloud where there is no CONUS RTMA.
7. Skip for now: NAM nests (stale, JPEG2000), RRFS (no feed), URMA (6 h late), HIRESW (not found), NBM `oc`.
