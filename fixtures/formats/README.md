# Fixtures for `crates/wx-formats`

Real files fetched on 2026-11-06 (sources and URLs in `docs/sources/radar-world.md`,
`satellite.md` and `canada-and-world.md`), whole unless the cut is stated.

| File | Product | Cut |
|---|---|---|
| `opera_20261106T1815_DBZH.h5`, `opera_20261106T1800_RATE.h5`, `opera_20261106T1800_ACRR.h5` | OPERA composites, `openradar-24h` bucket | whole |
| `dwd_composite_hx_20261106_1815-hd5` | DWD `composite/hx` LATEST | whole |
| `dwd_raa01-ry_20261106_1815-hdf5` | DWD `radolan/ry` latest | whole |
| `dwd_composite_wn_20261106_1815_cut4of25.tar` | DWD `composite/wn` LATEST tar | 4 of its 25 members (leads 000, 005, 060, 120), the original GNU tar entries copied byte for byte |
| `CMORPH2_0.25deg-30min_202611061700.RT.nc` | NOAA CPC CMORPH2 NRT | whole |
| `CMORPH2_0.25deg-30min_202611061630.RT.nc` | the half hour before, as rewritten at 20:18 UTC (used by `wx-ingest`'s CMORPH2 tests for an hourly total) | whole |
| `gmgsi_GLOBCOMPLIR_20261106T1700_rows700-1500_cols900-1700.nc` | GMGSI LW 17Z | rows 700 to 1499 and columns 900 to 1699 of `data`, `dqf`, `lat`, `lon`, rewritten with netCDF4-python (same names, attributes, shuffle and deflate) |
| `gmgsi_GLOBCOMPLIR_20261106T1700_rows0-460_cols2700-3950_nocoverage.nc` | the same file | rows 0 to 459, columns 2700 to 3949, cut the same way: the uncovered 60 to 72.7 N, 20 to 100 E (`dqf` 20) and row 0 (`dqf` fill) |
| `gmgsi_GLOBCOMPLIR_20261106T1700_rows1790-1830_cols1530-1570_count0.nc` | the same file | rows 1790 to 1829, columns 1530 to 1569: its two `dqf` 10 cells |
| `gmgsi_GLOBCOMPLIR_20261106T1700_rows0-120_cols2700-3100_nodqf.nc` | the same file | rows 0 to 119, columns 2700 to 3099, with `dqf` left out |
| `goes19_CMIPF_C13_s20263101810214_rows1200-1800_cols1800-2400.nc` | GOES-19 ABI-L2-CMIPF band 13, 18:10 scan | `CMI` rows 1200 to 1799, columns 1800 to 2399 (Gulf of Mexico); `x`/`y` `add_offset` moved to the first kept column and row |
| `goes19_CMIPF_C13_s20263101810214_rows2600-2800_cols0-300_limb.nc` | the same scan | rows 2600 to 2799, columns 0 to 299: the western limb, with off-disk fill |
| `dpc_SRI_20261106_1820.tif`, `dpc_VMI_20261106_1820.tif` | Italian DPC mosaic | whole |
| `jma_hrpns_20261106T1820_*.png` | JMA `hrpns` tiles `z_x_y`; `_empty` ones are the transparent RGBA tiles JMA serves where it is dry | whole |
| `jma_hrpns_nd_20261106T2030.geojson.gz` | JMA `hrpns_nd` coverage polygon (`.../surf/hrpns_nd/data.geojson?id=hrpns_nd`), gzipped as served | whole |
| `cwa_O-A0059-001_20261106T1820_cols420-660_rows0-300.json` | CWA radar composite | columns 420 to 659, rows 0 to 299; `StartPoint*` and `GridDimension*` rewritten to match |
| `synthetic_odim_claims_200000x200000.h5` | not real: the `ry` file's metadata with `where/xsize`, `ysize` set to 200000 over a 4 x 4 dataset, written with h5py | synthetic |

The full-size files the cuts come from back the ignored tests in
`crates/wx-formats/tests/samples.rs`.
