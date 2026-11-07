# Data sources

Every time step of every map layer is a mosaic of parts from the sources
below, the higher-ranked source on top where they overlap. This page lists
what the map uses, what it does not and why, and what would need an account.

The registry itself is code: one module per family under
`crates/wx-core/src/families/`, holding each source's row, what it feeds each
layer, where the data lives and what is still unverified. The research behind
it is in the other files of this directory, all fetched on 2026-11-06:
`noaa-regional.md`, `satellite.md`, `europe-models.md`, `canada-and-world.md`,
`radar-world.md` and `alerts-points-stations.md`. Anything marked UNVERIFIED
here is unverified there too.

## Adopted sources

Ranks run from 950 (top) to 50 (bottom): radar observations, then analyses,
radar nowcasts, fine models, regional models, global models, satellite
imagery and satellite rainfall. Forecast sources show only ahead of the newest
observation, except on the temperature, dew point, feels-like and wind
layers, where a model's recent hours also fill the past where no analysis
reaches.

### Observations

| Id | Provider | What it is | Coverage | Resolution | Cadence | Licence | Attribution to show |
|---|---|---|---|---|---|---|---|
| `dwd_radar` | DWD | Radar composite, column-maximum reflectivity (`hx`) | Germany and margins | 250 m | 5 min, 1-3 min late | CC BY 4.0 (DWD open data; terms page not retrieved) | Quelle: Deutscher Wetterdienst |
| `cwa` | Central Weather Administration, Taiwan | Composite reflectivity (O-A0059-001) | 18-29 N, 115-126.5 E | 0.0125° (1.4 km) | 10 min, 7 min late | Taiwan Open Government Data License | Central Weather Administration (Taiwan) |
| `jma_radar` | Japan Meteorological Agency | Precipitation nowcast analysis tiles, 8 rate classes | Japan | about 2 km (tiles at zoom 6) | 5 min, 1-2 min late | Public Data License v1.0 (Japan) | Source: Japan Meteorological Agency website (edited) |
| `mrms_guam` | NOAA/NSSL | MRMS radar mosaic and radar-only rainfall totals | Guam, 9-18 N, 140-150 E | 0.5 km | 2 min (24 h and 72 h totals hourly) | Public domain | NOAA/NSSL MRMS |
| `mrms_carib` | NOAA/NSSL | as above | Caribbean, 10-25 N, 90-60 W | 1 km | as above | Public domain | NOAA/NSSL MRMS |
| `mrms_hi` | NOAA/NSSL | as above | Hawaii, 15-26 N, 164-151 W | 0.5 km | as above | Public domain | NOAA/NSSL MRMS |
| `mrms_ak` | NOAA/NSSL | as above | Alaska, 50-72 N, 176-126 W | 1 km | as above | Public domain | NOAA/NSSL MRMS |
| `mrms` | NOAA/NSSL | as above | Contiguous US and southern Canada | 1 km | as above | Public domain | NOAA/NSSL MRMS |
| `dpc` | Dipartimento della Protezione Civile | Italian radar composite, vertical maximum reflectivity (VMI) | Italy and its seas | 1 km | 5 min, 7 min late | **CC BY-SA 4.0** | Dipartimento della Protezione Civile (CC BY-SA 4.0) |
| `opera` | EUMETNET | OPERA composite: maximum reflectivity; 1-hour rainfall | About 24 European countries; not Austria, Italy, Greece, Latvia, Bulgaria, Serbia, Turkey, Ukraine, Russia, Luxembourg; Spain in part | 1 km; 2 km for rainfall | 5 min; 15 min for rainfall | CC BY 4.0 | EUMETNET OPERA, contributing national meteorological services (wording UNVERIFIED) |
| `goes18` | NOAA | GOES-18 ABI band 13 (10.3 µm infrared), full disk | Pacific and western Americas, west of 106 W | 2 km | 10 min, 10 min late | Public domain | NOAA GOES-18 |
| `goes19` | NOAA | GOES-19 ABI band 13, full disk | Americas and Atlantic | 2 km | 10 min, 10 min late | Public domain | NOAA GOES-19 |
| `gmgsi` | NOAA/NESDIS | Global mosaic of geostationary longwave infrared (Meteosat-10, Meteosat-9, Himawari-9, GOES-18, GOES-19) | 72.7 S-72.7 N, all longitudes | 0.072° (about 8 km) | hourly, 35-40 min late | Public domain | NOAA/NESDIS OSPO GMGSI |
| `cmorph` | NOAA CPC | CMORPH2 satellite precipitation; 1 h, 24 h and 72 h totals summed from the half-hour files | Global | 0.25° (about 28 km) | 30 min, about 1 h late | Public domain | NOAA CPC CMORPH2 |

### Analyses

| Id | Provider | What it is | Coverage | Resolution | Cadence | Licence | Attribution to show |
|---|---|---|---|---|---|---|---|
| `rtma_gu` | NOAA/NCEP | Real-Time Mesoscale Analysis: 2 m temperature and dew point, 10 m wind and gust | Guam | 2.5 km | hourly, 35-40 min late | Public domain | NOAA/NCEP RTMA |
| `rtma_pr` | NOAA/NCEP | as above | Puerto Rico and US Virgin Islands | 1.25 km | as above | Public domain | NOAA/NCEP RTMA |
| `rtma_hi` | NOAA/NCEP | as above | Hawaii | 2.5 km | as above | Public domain | NOAA/NCEP RTMA |
| `rtma` | NOAA/NCEP | as above | Contiguous US | 2.5 km | hourly, about 50 min late | Public domain | NOAA/NCEP RTMA |
| `rtma_ak` | NOAA/NCEP | as above | Alaska and the North Pacific | 3 km | hourly, 35-40 min late | Public domain | NOAA/NCEP RTMA |

### Radar nowcasts

| Id | Provider | What it is | Coverage | Resolution | Cadence | Licence | Attribution to show |
|---|---|---|---|---|---|---|---|
| `jma_nowcast` | Japan Meteorological Agency | Precipitation nowcast to +60 min, 8 rate classes | Japan | about 2 km | 5-minute steps, new every 5 min | Public Data License v1.0 (Japan) | Source: Japan Meteorological Agency website (edited) |
| `dwd_nowcast` | DWD | Radar reflectivity nowcast (`wn`) to +120 min | Germany and margins | 1 km | 5-minute steps, new every 5 min | CC BY 4.0 (DWD open data) | Quelle: Deutscher Wetterdienst |

### Models

| Id | Provider | What it is | Coverage | Resolution | Runs and steps used | Licence | Attribution to show |
|---|---|---|---|---|---|---|---|
| `icon_d2` | DWD | ICON-D2 | Germany, Benelux, Switzerland, Austria, Czechia, Denmark, most of France, southern England, northern Italy | 2.2 km | every 3 h, hourly to 48 h; reflectivity every 15 min | CC BY 4.0 (DWD open data) | Quelle: Deutscher Wetterdienst |
| `nbm_gu` | NOAA/NWS | National Blend of Models | Guam | 2.5 km | hourly cycles, hourly to 48 h | Public domain | NOAA/NWS NBM |
| `nbm_pr` | NOAA/NWS | as above | Puerto Rico | 1.25 km | as above | Public domain | NOAA/NWS NBM |
| `nbm_hi` | NOAA/NWS | as above | Hawaii | 2.5 km | as above | Public domain | NOAA/NWS NBM |
| `arome_polyn` | Météo-France | AROME Outre-Mer | French Polynesia | 0.025° | 00/06/12/18 UTC, hourly to 48 h, 6.5-8 h late | Licence Ouverte 2.0 | Météo-France, with the run date |
| `arome_ncaled` | Météo-France | as above | New Caledonia | 0.025° | as above | Licence Ouverte 2.0 | Météo-France, with the run date |
| `arome_indien` | Météo-France | as above | Réunion, Mayotte and the south-west Indian Ocean | 0.025° | as above (possibly twice a day, UNVERIFIED) | Licence Ouverte 2.0 | Météo-France, with the run date |
| `arome_guyane` | Météo-France | as above | French Guiana | 0.025° | as above | Licence Ouverte 2.0 | Météo-France, with the run date |
| `arome_antilles` | Météo-France | as above | Lesser Antilles and Caribbean | 0.025° | as above | Licence Ouverte 2.0 | Météo-France, with the run date |
| `arome_fr` | Météo-France | AROME France | 37.5-55.4 N, 12 W-16 E | 0.025° | every 3 h, hourly to 51 h, 3-4 h late | Licence Ouverte 2.0 | Météo-France, with the run date |
| `ig` | DMI | HARMONIE IG | Greenland, Iceland, Svalbard | 2 km | runs every 3 h; one in six ingested (every 6 h, 3.1 GB each), hourly to 72 h | CC BY 4.0 | DMI HARMONIE (CC BY 4.0), modified |
| `dini` | DMI | HARMONIE DINI | North-west Europe from northern Spain to Finland, the UK, Ireland, Iceland | 2 km | runs every 3 h; one in six ingested (every 6 h, 3.7 GB each), hourly to 60 h | CC BY 4.0 | DMI HARMONIE (CC BY 4.0), modified |
| `hrdps` | ECCC | High Resolution Deterministic Prediction System | Canada (its grid also covers the northern US and eastern Alaska, which are masked out) | 2.5 km | 00/06/12/18 UTC, hourly to 48 h | ECCC Data Server End-use Licence v2.1.1 | Data Source: Environment and Climate Change Canada |
| `hrrr_ak` | NOAA/NCEP | HRRR Alaska | Alaska, Yukon, northern British Columbia | 3 km | every 3 h, hourly to 18 h (48 h at 00/06/12/18 UTC); reflectivity every 15 min to 18 h | Public domain | NOAA/NCEP HRRR |
| `hrrr` | NOAA/NCEP | High-Resolution Rapid Refresh | Contiguous US and southern Canada | 3 km | hourly, hourly to 18 h (48 h at 00/06/12/18 UTC); reflectivity every 15 min to 18 h | Public domain | NOAA/NCEP HRRR |
| `nbm_co` | NOAA/NWS | National Blend of Models | Contiguous US and southern Canada | 2.5 km | hourly cycles, hourly to 48 h | Public domain | NOAA/NWS NBM |
| `nbm_ak` | NOAA/NWS | as above | Alaska and the North Pacific | 3 km | as above | Public domain | NOAA/NWS NBM |
| `cptec` | CPTEC/INPE | WRF South America | 57.9 S-17.7 N, 90.7-19.4 W | 0.07° (about 7 km) | 00 UTC, hourly to 120 h (runs reach 180 h) | Not stated on the data server; the CPTEC site says CC BY-ND 3.0 (reach over the data UNVERIFIED) | CPTEC/INPE |
| `icon_eu` | DWD | ICON-EU | Europe, 29.5-70.5 N, 23.5 W-62.5 E | 0.0625° (about 6.5 km) | every 3 h, hourly to 78 h (30 h on the 03/09/15/21 UTC runs) | CC BY 4.0 (DWD open data) | Quelle: Deutscher Wetterdienst |
| `ifs` | ECMWF | IFS open data, **defined but not ingested yet** (3-hourly steps) | Global | 0.1° (about 9 km) | 4 runs a day, every 3 h to 144 h | CC BY 4.0 and the ECMWF Terms of Use | ECMWF (CC BY 4.0) (wording UNVERIFIED) |
| `gdps` | ECCC | Global Deterministic Prediction System | Global | 0.15° (about 15 km) | 00/12 UTC, hourly to 84 h (then 3-hourly to 240 h, not ingested) | ECCC Data Server End-use Licence v2.1.1 | Data Source: Environment and Climate Change Canada |
| `gfs` | NOAA/NCEP | Global Forecast System | Global | 0.25° | 4 runs a day, hourly to 120 h, then 3-hourly to 168 h | Public domain | NOAA/NCEP GFS |

### Where similar sources overlap

* HRDPS ranks above HRRR, HRRR Alaska and NBM, and its adapter blanks the
  United States out of its grid, so Canada shows HRDPS and the US its own
  models.
* The Alaska grids of RTMA and NBM reach Washington and British Columbia, so
  the CONUS rows rank above the Alaska rows.
* ICON-D2 ranks above AROME France and DINI. DINI's domain contains all of
  ICON-D2's, and ICON-D2 is the freshest of the three and the only one with
  its own reflectivity; this means ICON-D2 also covers much of France.
* GOES-18 is kept west of 106 W, so GOES-19 shows east of it; both are kept
  within 70° of their sub-satellite points, and GMGSI fills the rest of the
  world.
* On the radar layer, models without their own reflectivity get one derived
  from their precipitation rate. GDPS and IFS are left off that layer so
  that GFS's own simulated reflectivity is the global base.

## What the licences ask

* **Public domain** (NOAA): no conditions; the credit lines are courtesy.
* **CC BY 4.0** (DWD, DMI, EUMETNET OPERA, ECMWF): credit the source, link
  the licence and say the data were changed (the map resamples and recolours
  everything).
* **ECCC Data Server End-use Licence**: the credit "Data Source: Environment
  and Climate Change Canada", with a link to the licence
  (`https://eccc-msc.github.io/open-data/licence/readme_en/`) where possible.
* **Licence Ouverte 2.0** (Météo-France): name Météo-France and the date of
  the data's last update (the run time), and link the licence.
* **JMA Public Data License v1.0**: cite the JMA website and the page, say the
  content was edited, and do not present it as government output.
* **Taiwan Open Government Data License**: credit CWA.
* **CC BY-SA 4.0** (DPC Italy): share-alike. Tiles rendered from DPC data may
  have to be offered under the same licence. This is the same constraint that
  ruled out the Met Office data; it should be decided before DPC goes live.
* **CPTEC/INPE**: no licence found on the server. Credit CPTEC/INPE and ask
  them before relying on it commercially.

## Download volume

Measured or estimated from real file sizes on 6 November 2026, with every
source on and the default cycle spacings. The status page shows the live
figures per host.

| Source | About per day | Notes |
|---|---|---|
| DMI HARMONIE DINI + IG | 28 GB | 3.7 and 3.1 GB per run, one run in six ingested; set `INGEST_EVERY_HOURS` to 12 in their registry module to halve it |
| HRRR (lower 48 + Alaska) | 12 GB | hourly runs; `HRRR_MAX_HOUR=48` adds the long runs |
| MRMS (five domains) | 10 GB | radar every 2 minutes, totals every 10 |
| GOES-19 + GOES-18 | 7 GB | full disk every 10 minutes, about 25 MB each |
| GFS | 7 GB | four runs, byte ranges for our fields |
| AROME (France + 5 overseas) | 7 GB | France's packages are walked, overseas files downloaded whole |
| ICON-D2 | 4 GB | eight runs a day |
| ICON-EU | 3.5 GB | eight runs a day |
| NBM (five domains) | 3.4 GB | four of the 24 daily runs |
| HRDPS | 3.3 GB | four runs |
| GDPS | 2.4 GB | two runs, hourly to 84 h |
| Radar: OPERA, DWD, DPC, JMA, CWA | 3 GB | about 120 MB an hour, CWA's 9 MB JSON every 10 minutes the biggest |
| RTMA (five domains) | 1 GB | hourly |
| CMORPH2 | 0.7 GB | half-hourly files, re-read as CPC rewrites the last 12 hours |
| CPTEC | 0.6 GB | one run a day |
| GMGSI | 0.2 GB | hourly |
| Alerts, stations, point forecasts | small | conditional requests throughout |

Roughly 90 GB a day in all, with about 15 GB in the first half hour of a
cold start. Grids and tiles on disk settle around 12 to 15 GB.

## Not adopted, and why

* **ICON global** (DWD): icosahedral grid only, which needs the cell-centre
  files and a scatter-to-grid step; GDPS and IFS are better global bases.
* **The 3-hourly tails of the models** (ICON-EU past 78 h, NBM past 48 h, IFS
  for now): a 3-hourly source ranked above an hourly one hides the hourly
  steps and makes playback stutter. GFS is the exception, being at the
  bottom. GDPS past 84 h is three-hourly; CPTEC past 120 h is hourly but left out to
  keep volume down.
* **RDPS** (ECCC, 10 km): sits between HRDPS (finer, same provider) and GDPS
  (global, hourly), adding little.
* **ARPEGE** (Météo-France): IFS and GDPS cover the globe, AROME the regions;
  most of its parameter list was not checked.
* **AIFS** (ECMWF): 6-hourly, no gust and no precipitation type.
* **UKV and Met Office global**: NetCDF only, no wind components or
  precipitation type, and CC BY-SA 4.0 (share-alike). DINI and ICON cover the
  UK.
* **MEPS and AROME-Arctic** (MET Norway): OPeNDAP only, no file download.
* **ECCC radar**: coloured WMS images in 14 classes; no raw values anywhere.
* **Himawari-9 imagery**: its own binary format (bzip2 around JMA HSD) would
  need a custom reader; GMGSI already includes Himawari, hourly.
* **GK-2A**: level 1B only, uncompressed, and redundant with GMGSI.
* **NOAA Global Hydro-Estimator**: retired, no data since 2025-03-05.
* **GOES rain rate (RRQPEF)**: stalled since 2026-11-02 on both satellites,
  and it covers only the two GOES disks.
* **CMORPH V0.x RT 8 km**: flat binary, 60 S-60 N only; CMORPH2 supersedes
  it.
* **NAM nests**: no new data since 2026-11-03 and JPEG 2000 packing.
  **RRFS**: no live feed. **URMA**: about 6.7 h late. **HIRESW**: no public
  bucket found. **NBM `oc`**: no surface weather.
* **DWD RADOLAN rates and totals** (`ry`, `yw`, `rw`, `sf`): OPERA already
  gives Germany's rainfall totals; the reflectivity products are used.
* **SMHI, MeteoSwiss, FMI and MET Norway radar**: all inside OPERA.
* **SMN Argentina WRF** (4 km, CC BY 2.5 AR): usable, but not chosen in this
  round: NetCDF with wind speed and direction only, no gust, cloud,
  precipitation type or snow; CPTEC covers Argentina at 7 km.
* **CWA models**: the 3 km WRF has 6-hourly files only; the global model's
  files are 62 MB a step.
* **Open-Meteo bucket** (JMA MSM, CMA GRAPES and others): its own `.om`
  format, a lossy third-party copy, upstream licences not checked; MSM has no
  gust or precipitation type.
* **FMI HARMONIE**: a server-side subsetting API, overlapping DINI.
  **MeteoSwiss ICON-CH**: inside ICON-D2 and AROME. **GeoSphere Austria**:
  subset API only, and stale when checked.
* **BoM radar** (Australia): coloured images under restrictive terms.

## Needs a free account or key

None of these is used. Each would need the owner to register; some may cost
money for redistribution, which is noted where known.

| Service | What it would add | Sign-up |
|---|---|---|
| NASA Earthdata (IMERG) | Half-hourly global satellite precipitation, finer than CMORPH2 | https://urs.earthdata.nasa.gov/users/new |
| JAXA GSMaP | Hourly global satellite precipitation | https://sharaku.eorc.jaxa.jp/GSMaP/ ("User Registration") |
| EUMETSAT Data Store | Meteosat imagery every 10-15 min and MSG rain products for Europe, Africa, the Indian Ocean | account https://eoportal.eumetsat.int, key https://api.eumetsat.int/api-key |
| KNMI Data Platform | Dutch HARMONIE and radar | https://developer.dataplatform.knmi.nl/ |
| Météo-France API portal | Radar mosaics, including possibly overseas | https://portail-api.meteofrance.fr/ |
| Met Office DataHub | UK radar and model data | https://datahub.metoffice.gov.uk/ |
| AEMET OpenData | Spanish data (mostly text forecasts) | https://opendata.aemet.es/centrodedescargas/altaUsuario |
| MeteoGate developer portal | Higher limits on the OPERA API (the bucket used needs nothing) | https://devportal.meteogate.eu/ |
| KMA (Korea) | Korean radar and models | https://apihub.kma.go.kr/ and https://data.kma.go.kr/ |
| CWA open data portal (Taiwan) | Further CWA products (the radar used comes from the key-free bucket) | https://opendata.cwa.gov.tw/ |
| TMD (Thailand) | Thai radar | https://data.tmd.go.th/ |
| MET Malaysia | Malaysian radar | https://api.met.gov.my/ |
| REDEMET (Brazil) | Brazilian radar | https://redemet.decea.mil.br/ (API https://api-redemet.decea.mil.br/) |
| BoM Registered User Services | ACCESS models for Australia (registered users only) | https://www.bom.gov.au/catalogue/data-feeds.shtml |
| JMBSC | JMA GSM and MSM model grids (registration, paid) | https://www.jmbsc.or.jp/ |
| NCMRWF | Indian NCUM model grids | https://data.ncmrwf.gov.in/ |
| MetService and NIWA (New Zealand) | New Zealand radar and models | https://www.metservice.com/ and https://data.niwa.co.nz/ |
| ItaliaMeteo MeteoHub | Italian model grids (registration apparently needed, UNVERIFIED) | https://meteohub.mistralportal.it/ |
