# Satellite sources for global rain rate and cloud imagery

Research date: 2026-11-06, about 18:00-18:30 UTC. Everything below was fetched that day with
`User-Agent: weather-map-dev`, anonymous HTTPS, no account. Mark "UNVERIFIED" = not checked directly.
Samples are in `/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research/satellite/`
(not in the repo). S3 listing form used throughout:
`https://BUCKET.s3.amazonaws.com/?list-type=2&delimiter=/&max-keys=N&prefix=PREFIX`
(S3 listings are lexicographic, so the newest entry is the last one; use `start-after` or walk year/day/hour prefixes).

## Summary table

| Product | Usable | Coverage | Resolution | Cadence / measured latency | Container + filters | Size | Gotcha |
|---|---|---|---|---|---|---|---|
| NOAA GHE (`noaa-ghe-pds`) | NO, retired | 65N-65S | 4800x10020 | was 15 min; last object 2025-03-05 19:00Z | classic NetCDF-3 (`CDF\x01`) inside .gz | 2.4 MB gz / 192 MB raw | Dead on AWS for 20 months |
| CMORPH2 NRT, CPC (`ftp.cpc.ncep.noaa.gov`, HTTPS) | YES, best | global 90S-90N | 0.25 deg (720x1440) | 30 min; ~60 min after period start | HDF5/netCDF-4, sb v0, deflate L1, no shuffle, single chunk | ~0.6 MB | Not S3; files rewritten for 12 h as PMW data arrives; lon 0-360 |
| GOES-19/18 ABI-L2-RRQPEF | NOT NOW | disk, ~60 deg from subpoint | 2 km | 10 min; **no data since 2026-11-02 ~17:00Z** | HDF5 sb v2, shuffle+deflate5 | 1.7-2 MB | Stalled for both satellites; Americas/Pacific only anyway |
| GOES-19/18 ABI-L2-CMIPF C13 | YES | W. hemisphere (G19 75W, G18 137W) | 2 km | 10 min; ~10 min after scan start | HDF5 sb v2, shuffle+deflate5, int16 | 24.7 MB | Fixed-grid reprojection needed; only the Americas/Pacific/Atlantic |
| GOES CMIPF C02 (visible) | YES but heavy | same | 0.5 km, 21696^2 | 10 min | same filters, chunk (6,21696) | 432 MB | Far too big for a global layer |
| GOES MCMIPF | Not recommended | same | 2 km (all 16 bands) | 10 min | same | 379 MB | One file but 379 MB |
| Himawari-9 L1b HSD | YES (custom reader) | disk centred 140.7E | 2 km IR (R20), 1 km/0.5 km VIS | 10 min; segments land 11-14 min after scan start | bzip2 around JMA HSD binary (not NetCDF/HDF5) | B13: ~25 MB (10 segs) | Own binary format, 10 segments/band |
| Himawari-9 L2 (Clouds, ISatSS, Winds) | no rain product | disk | n/a | 10 min / ~18 min | HDF5 sb v0 | 85-745 MB | No rainfall rate exists in `noaa-himawari9` |
| GMGSI (`noaa-gmgsi-pds`) | YES, best imagery | lon -180..180, lat 72.7N-72.7S | 4999x3000, 0.072 deg Mercator | 1 hour; ~40 min after hour start | HDF5/netCDF-4 sb v2, shuffle+deflate5, float32 | 3.4-7.6 MB | Mercator rows; wrap column; 0-255 scaled, not Kelvin; VIS dark at night |
| GK2A AMI L1B (`noaa-gk2a-pds`) | marginal | 128.2E disk | 2 km IR (5500^2) | 10 min; 13-24 min | HDF5 sb v0, uncompressed uint16 DN | 35 MB per band | L1B only; no L2 rain rate on AWS |
| Meteosat via EUMETSAT | NO | Europe/Africa/IO | | | | | Data Store needs key; GMGSI already includes Meteosat-10 + Meteosat-9 IODC |
| NASA IMERG | NO | | | | | | Earthdata login |
| JAXA GSMaP | NO | | | | | | Registration needed |

## Recommendation

Rain rate base and totals: **CMORPH2 NRT 0.25 deg 30-minute files from CPC.** It is the only no-key
source verified that is global (90S-90N, essentially no gaps: 0.007% missing), is small (0.6 MB), has about
one hour latency, and is a plain HDF5 file with the simplest possible layout (superblock v0, one deflate chunk,
no shuffle). GHE is retired and the GOES RRQPE product is stalled and hemispheric. Rainfall totals for 1 h, 24 h, 72 h
are sums of the 30-minute rates (rate mm/h x 0.5 h): 2, 48, 144 files, so 1.2 MB, 29 MB, 85 MB of downloads on
a cold start, then incremental. Caveat: 0.25 deg is coarse (about 28 km), well below radar; it is a base layer.
CMORPH2 is a blend of microwave retrievals and IR, with latency refreshes, so re-fetch the last ~12 h occasionally.

Global imagery: **GMGSI** (LW IR, WV, SW IR, VIS) hourly, 7 MB files, full world including Meteosat and
Himawari sectors already merged. One file, one regular Mercator grid, easy to texture. Hourly cadence and a
40-minute latency are the cost. If 10-minute animation is wanted for the Americas, add GOES C13 (24 MB) on
top; for Asia/Pacific, Himawari HSD B13 (custom reader).

Decoder implications: every NetCDF file we would use is HDF5. Required filters in a pure-Rust reader: deflate
and shuffle (only). No szip, no fletcher32, no scale-offset filter anywhere. Superblock versions encountered are
v0 (CMORPH2, GK2A, Himawari L2) and v2 (GMGSI, GOES). The only classic NetCDF-3 file was GHE (retired).
So a small pure-Rust HDF5 reader covering superblock v0 and v2, object headers v1 and v2, chunked layout with
deflate+shuffle is sufficient. Chunk-index type for the v2 files (v1 B-tree vs v2 B-tree/fixed array) is UNVERIFIED.

## 1. NOAA Global Hydro-Estimator (GHE) - RETIRED

- Bucket `noaa-ghe-pds`, key `rain_rate/YYYY/MM/DD/NPR.GEO.GHE.v1.SYYYYMMDDHHMM.nc.gz` (15-minute files).
- Newest object in the bucket: `rain_rate/2025/03/05/NPR.GEO.GHE.v1.S202503051900.nc.gz` (2025-03-05). No later months exist
  (2025 has only 01, 02, 03). STAR page `https://www.star.nesdis.noaa.gov/smcd/emb/ff/HydroEst.php` says "Imminent Product
  Retirement and Data Location Changes - February 2025" and points at the same bucket, so no replacement location is published.
  Verdict: not usable for live data. (Also no 1 h/3 h/6 h/24 h totals in the bucket, only `rain_rate/`.)
- Sample verified: 200 `https://noaa-ghe-pds.s3.amazonaws.com/rain_rate/2025/03/05/NPR.GEO.GHE.v1.S202503051900.nc.gz`, 2,352,629 bytes.
- Container: gzip (`1f 8b`), inside `CDF\x01` = classic NetCDF-3, NETCDF3_CLASSIC. Uncompressed 192,384,356 bytes. Easy pure Rust, but moot.
- Variable `rain` float32 (4800 lines, 10020 elems), units `mm` (long_name "GHE Global Instantaneous rain total"; effectively mm/h),
  `_FillValue` -9999, valid_range 0..508, `grid_range` "Lat 65 to -65, Lon -180 to +180". 48% of pixels fill, 49% zero.
  Lat/lon come from a separate `NPR.GEO.GHE.v1.Navigation.netcdf(.gz)` (170 MB gz); not downloaded (grid layout UNVERIFIED).

## 2. GOES-19 (East, 75W) and GOES-18 (West, 137W) ABI

Buckets `noaa-goes19`, `noaa-goes18`. Key layout: `PRODUCT/YYYY/DDD/HH/OR_PRODUCT-M6[Cnn]_G19_sYYYYDDDHHMMSSs_eYYYYDDDHHMMSSs_cYYYYDDDHHMMSSs.nc`
(DDD = day of year; 2026-11-06 = 310). Mode `M6` = 10-minute full disk. Listing one hour prefix gives at most 6 files (one band)
so "newest" = list `PRODUCT/YYYY/DDD/HH/` for the current and previous hour and take the last key. Public domain (NOAA Open Data).

### ABI-L2-RRQPEF (Rainfall Rate QPE, full disk)

- **Stalled.** Last object on both satellites: G19 `ABI-L2-RRQPEF/2026/306/16/OR_ABI-L2-RRQPEF-M6_G19_s20263061640209_e20263061649518_c20263061649569.nc`
  (written 2026-11-02 16:50Z); G18 `..._G18_s20263061650216...` (2026-11-02 17:00Z). Days 307-310 exist as empty prefixes. UNVERIFIED whether temporary.
  Earlier continuous (e.g. 2026-10-27 12:00-13:00Z present). While running it is a 10-min cadence, ~1 minute latency, 1.4-2.0 MB per file.
- Sample (200): `https://noaa-goes19.s3.amazonaws.com/ABI-L2-RRQPEF/2026/306/04/OR_ABI-L2-RRQPEF-M6_G19_s20263060450211_e20263060459520_c20263060459571.nc`, 1,749,480 bytes.
- Container: `\x89HDF\r\n\x1a\n`, superblock version 2, netCDF-4 (new-style groups/links).
- `RRQPE` uint16 (5424,5424), chunks (24,5424), filters shuffle then deflate level 5, `scale_factor` 0.00152602, `add_offset` 0,
  `_FillValue` 65535, valid_range 0..65530, units `mm h-1`. 75.9% zeros (no rain), 2.4% raining, 21.7% fill (off-disk / beyond LZA limit). Max 100 mm/h (cap).
- `DQF` uint8 (5424,5424) chunks (48,5424), shuffle+deflate5, `_FillValue` 255; values 0 good, 3 (degraded/LZA or invalid) ; flag bit meanings in attrs.
- Projection (`goes_imager_projection`): geostationary, perspective_point_height 35786023 m, semi_major_axis 6378137, semi_minor_axis 6356752.31414,
  inverse_flattening 298.2572221, lat origin 0, lon origin -75.0 (G19) / -137.0 (G18), sweep_angle_axis `x`.
  `x` int16 scale 5.6e-05 add -0.151844 rad; `y` int16 scale -5.6e-05 add 0.151844 rad (north-up, row 0 north); nx = ny = 5424; 2 km at nadir.

### ABI-L2-CMIPF (Cloud and Moisture Imagery, one band per file)

- Key: `ABI-L2-CMIPF/2026/310/18/OR_ABI-L2-CMIPF-M6C13_G19_s20263101810214_e20263101819534_c20263101820006.nc`.
  Cadence 10 min; the file for a scan starting 18:10:21 had object time 18:20:18Z, i.e. about 10 min after scan start (at the moment of
  the check, the newest object was less than a minute old). Latency is essentially the scan duration.
- Verified 200 (downloaded, 24,684,740 bytes): G19 C13 above. G18 equivalent `ABI-L2-CMIPF/2026/310/18/OR_ABI-L2-CMIPF-M6C13_G18_s20263101810223_e20263101819543_c20263101820018.nc`, 24,524,861 bytes.
- Container: HDF5 superblock v2 (`89 48 44 46 0d 0a 1a 0a`, version byte 02).
- C13 (10.3 um clean IR, 2 km): `CMI` int16 (5424,5424), chunks (24,5424), shuffle+deflate5, `scale_factor` 0.06145332, `add_offset` 89.62, `_FillValue` -1,
  valid_range 0..4095, units `K` (brightness temperature = DN*scale+offset). `DQF` int8 chunks (48,5424). Same x/y/projection as above (lon -75 or -137).
- C02 (0.64 um visible, 0.5 km): `CMI` int16 (21696,21696), chunks (6,21696), shuffle+deflate5, `scale_factor` 0.00031746, add 0, units `1` (reflectance factor, up to ~1.3),
  x/y scale 1.4e-05, add -0.151865 / 0.151865. Metadata read by HTTP range (file not downloaded). File size 432 MB (e.g. `.../OR_ABI-L2-CMIPF-M6C02_G19_s20263101750214_..._c20263101759582.nc` = 429,458,597 B; the 17:00 scan file was 432,586,379 B).
  Other sizes (G19, 17Z): C01 94 MB, C03 103 MB, C04 15 MB, C05 98 MB, C06 22.5 MB, C07 27.6 MB.
- MCMIPF (all 16 bands on the 2 km grid in one file): `ABI-L2-MCMIPF/2026/310/17/OR_ABI-L2-MCMIPF-M6_G19_s20263101750214_e20263101759533_c20263101800013.nc`, 378.5 MB, written ~10 min after scan start.
- Coverage: only the disk seen by each satellite (about 60 degrees of great circle from the subpoint); G19 covers the Americas and Atlantic,
  G18 the Pacific. They don't help with Europe, Africa, Asia.

## 3. Himawari-9 (`noaa-himawari9`)

Top-level prefixes: `AHI-L1b-FLDK/`, `AHI-L1b-Japan/`, `AHI-L1b-Target/`, `AHI-L2-FLDK-Clouds/`, `AHI-L2-FLDK-ISatSS/`, `AHI-L2-FLDK-Winds/`.
**No rainfall rate product exists in this bucket.** (Himawari-8 is not in this bucket; no `noaa-himawari8` check was made: UNVERIFIED.)

- L1b full disk: raw JMA HSD, bzip2 compressed. Key `AHI-L1b-FLDK/YYYY/MM/DD/HHMM/HS_H09_YYYYMMDD_HHMM_Bnn_FLDK_Rrr_Sssss.DAT.bz2`, `Rrr` = R05 (0.5 km),
  R10 (1 km), R20 (2 km); `Sssss` = segment `S0110`..`S1010` (10 N-S segments). 10-minute cadence (dir names ...00, 10, 20...), HHMM = scan start.
  For the 18:00 scan: B13 segments written 18:11:55-18:13:37 (about 12-14 min after scan start; the scan itself takes 10 min).
- Verified 200: `https://noaa-himawari9.s3.amazonaws.com/AHI-L1b-FLDK/2026/11/06/1800/HS_H09_20261106_1800_B13_FLDK_R20_S0510.DAT.bz2`, 3,076,754 bytes; bytes `BZh9`; after bzcat the header begins `01 1a 01 0b ...` then ASCII "Himawari-9", "MSC" (HSD header block 1). Sizes for a full B13 (10 segments): 1.2, 2.2, 2.6, 2.8, 3.1, 3.0, 2.7, 2.6, 2.1, 1.2 MB = ~24 MB. B03 (R05) segments are 9-18+ MB each.
- Not NetCDF, not HDF5. A pure-Rust reader needs: bzip2 decompress, parse the HSD headers (basic info, data info, projection info, navigation, calibration), counts to radiance to Tb via the supplied tables. Projection details not read from the sample (UNVERIFIED); AHI full disk is geostationary at 140.7E, IR 5500x5500 at 2 km (standard HSD spec).
- L2 NetCDF (HDF5 superblock v0, bytes `89 48 44 46 0d 0a 1a 0a 00 00 00 00`): `AHI-L2-FLDK-Clouds/2026/11/06/1800/AHI-CHGT_...nc` (cloud height, ~745 MB), `AHI-CMSK_...nc` (mask, 352 MB), `AHI-CPHS_...nc` (phase, 85 MB), written ~18 min after scan start.
  `AHI-L2-FLDK-ISatSS/` are small tiled products (0.1-3 MB, `OR_HR3-...-T001_GH9_...nc`, ~every 2-3 min). `AHI-L2-FLDK-Winds/` is a winds product at 10-min cadence. None useful for rain.

## 4. GMGSI (`noaa-gmgsi-pds`)

- Prefixes `GMGSI_LW/` (longwave IR, ~11 um), `GMGSI_SW/` (shortwave IR 3.9 um), `GMGSI_WV/` (water vapour 6.2 um), `GMGSI_VIS/` (visible), `GMGSI_SSR/` (exists, empty for today).
- Key: `GMGSI_LW/YYYY/MM/DD/HH/GLOBCOMPLIR_v3r0_blend_sYYYYMMDDHH00000_eYYYYMMDDHH09599_cYYYYMMDDHHMMSSf.nc` (prefix names: LIR, SIR, WV, VIS -> GLOBCOMPLIR, GLOBCOMPSIR, GLOBCOMPWV, GLOBCOMPVIS). One file per hour per channel. The `c` creation stamp has a random last digit, so list the hour prefix to find the key.
- Latency: file for 17:00Z written 17:33:47-17:39:09Z (about 35-40 min after hour start); at 18:17 the newest was still the 17Z one.
  Newest discovery: list `GMGSI_LW/YYYY/MM/DD/` (delimiter, returns hour dirs) and take the last; confirm the hour dir has an object.
- Verified 200 (all downloaded):
  - LW `GMGSI_LW/2026/11/06/17/GLOBCOMPLIR_v3r0_blend_s202611061700000_e202611061709599_c202611061733476.nc` 7,315,638 B
  - VIS `GMGSI_VIS/2026/11/06/17/GLOBCOMPVIS_v3r0_blend_s202611061700000_e202611061709599_c202611061739001.nc` 7,360,216 B
  - WV `GMGSI_WV/2026/11/06/17/GLOBCOMPWV_v3r0_blend_s202611061700000_e202611061709599_c202611061737007.nc` 3,413,253 B
  - SW `GMGSI_SW/2026/11/06/17/GLOBCOMPSIR_v3r0_blend_s202611061700000_e202611061709599_c202611061735275.nc` 7,569,407 B
- Container: HDF5 netCDF-4, superblock version 2 (`89 48 44 46 0d 0a 1a 0a 02 08 08 00`), written by NCO 4.7.5.
- Variables: `data` float32 (1,3000,4999) chunks (1,793,1322) (so 4x4 = 16 chunks, last ones partial), filters shuffle then deflate level 5.
  `dqf` int8 same dims, chunks (1,1586,2644), shuffle+deflate5, `_FillValue` -128, values seen -128, 0, 10, 20.
  `lat` and `lon` float32 (3000,4999), chunks whole array, shuffle+deflate5 (redundant 2D arrays, ~60 MB uncompressed each; ignore and compute).
  `time` int32 (1,) seconds since 1970 (1793984400 = 2026-11-06T17:00:00Z). `xc`, `yc` are all-zero dummies. No scale_factor/add_offset on `data`.
- Value encoding: `data` is a 0-255 enhanced count (long_name "0-255 Brightness Temperature", attr says units `K` but values are 0..255, NOT Kelvin; mapping from
  count to temperature/reflectance is UNVERIFIED, treat as ready-to-display grayscale; whether 255 is cold or warm for LW also UNVERIFIED). `_FillValue` -9999 (no fill pixels seen; no NaN). VIS: night is value 0 (36% flagged bad in the DQF stat at 17Z).
  Mean values at 17Z: LW 131, SW 118, WV 187, VIS 49.
- Grid (verified from lat/lon arrays): **Mercator**, not plate carree. 4999 columns x 3000 rows.
  Columns: col 0 has lon +180.0 (a wrap duplicate of the dateline), cols 1..4998 have lon = -179.9284 + (i-1)*0.07201 (col 4998 = 179.9004). dx = 0.0720 deg.
  Rows: row 0 north. lat = degrees(atan(sinh((1499 - j) * 0.072 deg in radians))); row 1499 is lat 0 (1.5e-5), row 0 is 72.7154N, row 2999 is 72.7368S (dlat 0.0214 deg at the poles, 0.0720 at the equator).
  Attrs say lat_resolution 0.0722, lon_resolution 0.0722 (approximate).
- Sources: Meteosat-10 (0E), Meteosat-9 IODC (45.5E), Himawari-9, GOES-West (G18), GOES-East (G19) are blended (`platform` attr "Meteosat10,G18,H-9,G19,Meteosat9"; `instrument` "MSG-SEVIRI,Himawari-AHI,GOES-ABI"). This answers the "no-key Europe/Africa/Indian Ocean imagery" question.
- Licence: NOAA/NESDIS OSPO product, US government work, no restrictions stated in the files (contact attrs exist; do not copy). Credit "NOAA/NESDIS OSPO GMGSI".

## 5. GEO-KOMPSAT-2A and Meteosat

- `noaa-gk2a-pds`: prefixes `AMI/L1B/` (`FD/` full disk, `LA/` local area), `GK2A/LE1B/`, `GK2A/LV1/`, `LV1/` (annual `NA/data/` files such as `LV1/PD-E-5M/NA/data/gk2a_ksem_pd_e_5m_le1_20230601.nc`, space-weather instrument data) and `GK2A_LA_TargetList.txt`. `AMI/L2/` is empty. **No rainfall rate (L2) on AWS.**
- Key: `AMI/L1B/FD/YYYYMM/DD/HH/gk2a_ami_le1b_ir105_fd020ge_YYYYMMDDHHMM.nc` (bands vi004..., ir087, ir105...; `fd020ge` = full disk, 2 km, "ge" = geostationary), 10-minute cadence, written 13-24 min after nominal time.
  Verified 200: `https://noaa-gk2a-pds.s3.amazonaws.com/AMI/L1B/FD/202611/06/18/gk2a_ami_le1b_ir105_fd020ge_202611061800.nc`, 34,965,564 B (written 18:23:09Z).
- Container: HDF5, superblock version 0 (old-style groups, symbol tables). `image_pixel_values` uint16 (5500,5500), chunked (1375,1375), **no filters**; raw DN, calibration in attrs
  (`DN_to_Radiance_Gain/Offset`, `Teff_to_Tbb_c0..c2`, `Plank_constant_h` etc., plus GSICS coefficients). Projection attrs: `projection_type` GEOS, `sub_longitude` 2.2375 rad (128.2E), `nominal_satellite_height` 42164000 m, `cfac/lfac/coff/loff` (CGMS LRIT-style pixel scaling), 5500x5500. Coverage East Asia/Australia/West Pacific: overlaps Himawari. Redundant with GMGSI.
- Meteosat (EUMETSAT): Data Store `https://data.eumetsat.int` requires a free account + OAuth key (consumer key/secret from `https://api.eumetsat.int/api-key`). The catalogue browse endpoint `https://api.eumetsat.int/data/browse/collections` answered anonymously (200 JSON), but downloads need a token (UNVERIFIED by test; per EUMETSAT docs). Not used. No other anonymous Meteosat feed found; GMGSI is the answer for Europe/Africa/Indian Ocean imagery. No no-key geostationary rain rate for those regions was found.

## 6. Other global precipitation estimates

### CMORPH2 near-real-time (CPC/NCEP) - recommended

- Index `https://ftp.cpc.ncep.noaa.gov/precip/CMORPH2/CMORPH2NRT/` (plain HTTPS on the FTP host; Apache directory listing). Readme (verified): `readme_CMORPH2x_NRT.txt`.
- Key: `.../DATA/YYYY/YYYYMM/YYYYMMDD/CMORPH2_0.25deg-30min_YYYYMMDDHHMM.RT.nc`; HHMM is the START of the 30-minute period (e.g. 1700 = 17:00:00-17:29:59Z). Verified 200:
  `https://ftp.cpc.ncep.noaa.gov/precip/CMORPH2/CMORPH2NRT/DATA/2026/202611/20261106/CMORPH2_0.25deg-30min_202611061700.RT.nc`, 588,666 B (and `...202611061730.RT.nc`, 591,916 B, last-modified 18:15:19Z, listed by 18:29Z).
  Archive back to 2023 exists (`DATA/2023..2026`). Only 0.25 deg files are published (native 0.05 deg is not).
- Latency: documented ONE HOUR from target time, then each field is re-generated every 30 min for 12 h as more PMW retrievals arrive (last-modified times of the last several files jump together, e.g. 17:45 then 18:15). Observed: at 18:29Z the newest file was 17:30 start (about 59 min after period start, 29 min after period end). Directory listings may lag a few minutes (one listing at 18:26 still showed 17:00 as newest).
  Future dated day/month directories are pre-created and empty (e.g. `202611/20261130/`), so always take the newest file inside the current day, not the last directory.
- Container: `89 48 44 46 0d 0a 1a 0a 00 00 00 00` = HDF5, superblock v0, netCDF-4 (`_NCProperties version=2,netcdf=4.7.0,hdf5=1.10.5`).
- `cmorph` int16 (1,720,1440), single chunk (1,720,1440), filter deflate level 1 only (no shuffle, no fletcher). `scale_factor` 0.01, `add_offset` 0, `missing_value` -999, valid 0..32767, units `mm/hr`, standard_name `lwe_precipitation_rate`. 76.3% zeros, 13.6% of nonzero >0.1 mm/h; missing 0.007% (only top row). Max seen 62.77.
  `frac` int16 (same shape/layout) = percentage of frozen precipitation, scale 0.01 (0..100), missing -999.
- Grid: `lat` float32 (720) from -89.875 to +89.875 ascending (row 0 = south, flip for north-up), `lon` float32 (1440) cell centres 0.125..359.875 (**0-360**, wrap at 180 for a -180..180 map), 0.25 deg. `time` int32 seconds since 1970 (period start).
- Licence: NOAA CPC product, US government work, public domain; contact attr/readme names individual scientists, do not copy. Credit "NOAA CPC CMORPH2".
- Related CPC items checked: `CMORPH_RT/GLOBE/data/YYYY/YYYYMM/CMORPH_V0.x_RT_8km-30min_YYYYMMDDHH.gz` (older CMORPH V0.x RT, updated hourly, 2.6 MB gz; a gzip of a flat little-endian float32 binary, 4948x1649, 8 km, lon from 0.0364 step 0.07276, lat -59.96 to +59.96 step 0.07277, UNDEF -999, two 30-min fields per hourly file: 65,274,016 bytes decompressed; GrADS `.ctl` in `CMORPH_RT/GLOBE/data/CTL/`) is 60S-60N only; worth keeping as a fallback and for higher resolution, but needs a flat-binary reader and CMORPH2 supersedes it.
  `global_full_res_IR/merg_YYYYMMDDHH_4km-pixel.Z` (compress-wrapped merged global 4 km IR) is only updated daily at about 00:06Z (newest file 2026-11-04 23Z): not near real time. `global_MWCOMB/30min_8km` stopped 2025-06-23. NCEI CMORPH archive (`ncei.noaa.gov/data/cmorph-high-resolution-global-precipitation-estimates`) lags by months (newest month May 2026).
- JPSS MiRS microwave rain-rate swaths exist in `noaa-nesdis-n20-pds`, `-snpp-pds`, `-n21-pds` (`NPR_MIRS_IMG/`), orbital swaths not a gridded mosaic; not investigated further (UNVERIFIED).

### Key-gated (not registered; for the record)

- NASA IMERG (Early/Late half-hourly): needs NASA Earthdata Login, free sign-up `https://urs.earthdata.nasa.gov/users/new`. `https://jsimpson.pps.eosdis.nasa.gov/` answered 401; GES DISC `gpm1.gesdisc.eosdis.nasa.gov` data also requires Earthdata auth (landing page returned a banner only).
- JAXA GSMaP realtime: registration for FTP at `https://sharaku.eorc.jaxa.jp/GSMaP/` ("User Registration" link). Anonymous `ftp://hokusai.eorc.jaxa.jp/pub/` holds only TRMM/ftp2/usr, no GSMaP.
- EUMETSAT Data Store (Meteosat imagery, MSG rain products): free account at `https://eoportal.eumetsat.int`, API key at `https://api.eumetsat.int/api-key`.

## Open items / UNVERIFIED

- Whether GOES RRQPEF resumes (stopped 2026-11-02 ~17Z for both satellites). Even when running, it covers only the two GOES disks.
- HDF5 object-header and chunk-index sub-versions of the superblock-v2 files (GMGSI, GOES). Probably v2 object headers with v1 or v4 layout messages; check with the real decoder.
- GMGSI count-to-temperature mapping and polarity; GMGSI_SSR meaning (empty).
- GHE Navigation file layout (retired product).
- Himawari HSD projection/calibration block contents (header read only).
- Whether Himawari-8 data is anywhere in `noaa-himawari9`: not checked.
- CMORPH2 first-write time vs the 12-hour refreshes (file modified times change after the first write).
