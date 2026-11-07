// Synthetic alerts from the rest of the world, shaped like what the server's
// CAP path stores: MeteoAlarm countries whose shapes are unions of NUTS 3
// regions, national services' own CAP polygons, and alerts that name only
// region codes and so have no shape. Shapes are drawn by hand, coarse but in
// the right places. The texts deliberately cover what the panel must cope
// with: several languages, right-to-left scripts, CJK, very long German
// compounds, and an alert with most optional fields missing.

import type { Severity } from '../src/api/types.ts';
import type { Polygon } from './mvt.ts';

/** A WMO register entry, as `/api/alerts/sources` describes it. */
export interface WorldSource {
  id: string;
  name: string;
  country: string;
  country_name: string;
  /** WMO Regional Association, 1 to 6. */
  region: number;
  feed: string;
}

interface WorldInfo {
  language: string;
  event: string;
  headline?: string;
  description?: string;
  instruction?: string;
  area?: string;
  sender?: string;
  web?: string;
  /** A block that no longer counts (an earlier time step, urgency Past). */
  past?: true;
}

export interface WorldTemplate {
  /** Seed of the alert's id: keep it fixed so ids survive restarts. */
  n: number;
  source: string;
  geom: 'cap' | 'nuts' | 'none';
  /** Longitudes may run past 180 for a shape across the antimeridian; it is
   *  split there when the alert is built. Empty for `none`. */
  polygons: Polygon[];
  /** The register's point for the country, for an alert without a shape. */
  anchor?: [number, number];
  severity: Severity;
  urgency: string | null;
  certainty: string | null;
  category: string | null;
  /** Hours relative to now. */
  sent: number;
  onset: number;
  /** `null` when the sender gave no expiry: the server assumes a day after
   *  onset. */
  expires: number | null;
  infos: WorldInfo[];
}

const MA = 'https://feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-';

/** The Kenyan alert, without a shape: the mock ends and restores it. */
export const UNSHAPED_SEED = 1035;

export const WORLD_SOURCES: WorldSource[] = [
  { id: 'es-aemet-es', name: 'Spain: Agencia Estatal de Meteorología', country: 'es', country_name: 'Spain', region: 6, feed: `${MA}spain` },
  { id: 'fr-meteofrance-fr', name: 'France: Météo-France', country: 'fr', country_name: 'France', region: 6, feed: `${MA}france` },
  { id: 'pt-ipma-pt', name: 'Portugal: Instituto Português do Mar e da Atmosfera', country: 'pt', country_name: 'Portugal', region: 6, feed: `${MA}portugal` },
  { id: 'at-geosphere-de', name: 'Austria: GeoSphere Austria', country: 'at', country_name: 'Austria', region: 6, feed: `${MA}austria` },
  { id: 'pl-imgw-pl', name: 'Poland: Institute of Meteorology and Water Management', country: 'pl', country_name: 'Poland', region: 6, feed: `${MA}poland` },
  { id: 'ro-anm-ro', name: 'Romania: National Meteorological Administration', country: 'ro', country_name: 'Romania', region: 6, feed: `${MA}romania` },
  { id: 'gr-hnms-el', name: 'Greece: Hellenic National Meteorological Service', country: 'gr', country_name: 'Greece', region: 6, feed: `${MA}greece` },
  { id: 'nl-knmi-nl', name: 'Netherlands: Royal Netherlands Meteorological Institute', country: 'nl', country_name: 'Netherlands', region: 6, feed: `${MA}netherlands` },
  { id: 'hr-dhmz-hr', name: 'Croatia: Meteorological and Hydrological Service', country: 'hr', country_name: 'Croatia', region: 6, feed: `${MA}croatia` },
  { id: 'cz-chmi-cs', name: 'Czechia: Czech Hydrometeorological Institute', country: 'cz', country_name: 'Czechia', region: 6, feed: `${MA}czechia` },
  { id: 'it-meteoam-it', name: 'Italy: Aeronautica Militare, Servizio Meteorologico', country: 'it', country_name: 'Italy', region: 6, feed: `${MA}italy` },
  { id: 'no-metno-no', name: 'Norway: Norwegian Meteorological Institute', country: 'no', country_name: 'Norway', region: 6, feed: 'https://api.met.no/weatherapi/metalerts/2.0/current.rss' },
  { id: 'de-dwd-de', name: 'Germany: Deutscher Wetterdienst', country: 'de', country_name: 'Germany', region: 6, feed: 'https://www.dwd.de/DWD/warnungen/cap-feed/de/atom.xml' },
  { id: 'ch-meteoswiss-en', name: 'Switzerland: Federal Office of Meteorology and Climatology MeteoSwiss', country: 'ch', country_name: 'Switzerland', region: 6, feed: 'https://www.meteoswiss.admin.ch/cap/rss.xml' },
  { id: 'is-imo-en', name: 'Iceland: Icelandic Meteorological Office', country: 'is', country_name: 'Iceland', region: 6, feed: 'https://api.vedur.is/cap/v1/capbroker/active/category/Met/rss' },
  { id: 'il-ims-he', name: 'Israel: Israel Meteorological Service', country: 'il', country_name: 'Israel', region: 6, feed: 'https://ims.gov.il/sites/default/files/ims_data/rss/alert/rssAlert_general_country_en.xml' },
  { id: 'ca-msc-xx', name: 'Canada: Meteorological Service of Canada', country: 'ca', country_name: 'Canada', region: 4, feed: 'https://rss.naad-adna.pelmorex.com/' },
  { id: 'mx-smn-es', name: 'Mexico: Servicio Meteorológico Nacional', country: 'mx', country_name: 'Mexico', region: 4, feed: 'https://smn.conagua.gob.mx/tools/PHP/feedsmn/cap.php' },
  { id: 'br-inmet-pt', name: 'Brazil: Instituto Nacional de Meteorologia', country: 'br', country_name: 'Brazil', region: 3, feed: 'https://apiprevmet3.inmet.gov.br/avisos/rss' },
  { id: 'au-bom-en', name: 'Australia: Bureau of Meteorology', country: 'au', country_name: 'Australia', region: 5, feed: 'https://www.bom.gov.au/fwo/IDZ00059.warnings_cap.xml' },
  { id: 'nz-metservice-en', name: 'New Zealand: MetService', country: 'nz', country_name: 'New Zealand', region: 5, feed: 'https://alerts.metservice.com/cap/rss' },
  { id: 'fj-fms-en', name: 'Fiji: Fiji Meteorological Service', country: 'fj', country_name: 'Fiji', region: 5, feed: 'https://www.met.gov.fj/cap/rss.xml' },
  { id: 'ph-pagasa-en', name: 'Philippines: Philippine Atmospheric, Geophysical and Astronomical Services Administration', country: 'ph', country_name: 'Philippines', region: 5, feed: 'https://publicalert.pagasa.dost.gov.ph/feeds/' },
  { id: 'id-bmkg-id', name: 'Indonesia: Agency for Meteorology, Climatology and Geophysics', country: 'id', country_name: 'Indonesia', region: 5, feed: 'https://signature.bmkg.go.id/alert/public/en/rss.xml' },
  { id: 'jp-jma-ja', name: 'Japan: Japan Meteorological Agency', country: 'jp', country_name: 'Japan', region: 2, feed: 'https://www.data.jma.go.jp/developer/xml/feed/extra.xml' },
  { id: 'kr-kma-ko', name: 'Republic of Korea: Korea Meteorological Administration', country: 'kr', country_name: 'Republic of Korea', region: 2, feed: 'https://www.weather.go.kr/w/rss/cap.do' },
  { id: 'hk-hko-en', name: 'Hong Kong, China: Hong Kong Observatory', country: 'hk', country_name: 'Hong Kong, China', region: 2, feed: 'https://alerts.weather.gov.hk/V1/cap_atom.xml' },
  { id: 'in-imd-en', name: 'India: India Meteorological Department', country: 'in', country_name: 'India', region: 2, feed: 'https://sachet.ndma.gov.in/cap_public_website/rss/rss_india.xml' },
  { id: 'ae-ncm-en', name: 'United Arab Emirates: National Center of Meteorology', country: 'ae', country_name: 'United Arab Emirates', region: 2, feed: 'https://www.ncm.gov.ae/cap/rss' },
  { id: 'sa-ncm-ar', name: 'Saudi Arabia: National Center for Meteorology', country: 'sa', country_name: 'Saudi Arabia', region: 2, feed: 'https://ncm.gov.sa/cap/rss.xml' },
  { id: 'za-saws-en', name: 'South Africa: South African Weather Service', country: 'za', country_name: 'South Africa', region: 1, feed: 'https://caps.weathersa.co.za/Home/RssFeed' },
  { id: 'ke-kmd-en', name: 'Kenya: Kenya Meteorological Department', country: 'ke', country_name: 'Kenya', region: 1, feed: 'https://meteo.go.ke/cap/rss.xml' },
];

/** Sources the register lists that have nothing current, or whose feed is
 *  failing, so the sources answer has every state in it. */
export const QUIET_SOURCES: (WorldSource & { state: 'empty' | 'failing' | 'starting'; error?: string })[] = [
  { id: 'kw-dom-en', name: 'Kuwait: Meteorological Department', country: 'kw', country_name: 'Kuwait', region: 2, feed: 'https://www.met.gov.kw/rss_eng/kuwait_cap.xml', state: 'failing', error: 'feed: timed out' },
  { id: 'eg-ema-ar', name: 'Egypt: Egyptian Meteorological Authority', country: 'eg', country_name: 'Egypt', region: 1, feed: 'https://ema.gov.eg/cap/rss.xml', state: 'failing', error: 'feed: 503 Service Unavailable' },
  { id: 'ma-dmn-fr', name: 'Morocco: Direction Générale de la Météorologie', country: 'ma', country_name: 'Morocco', region: 1, feed: 'https://www.marocmeteo.ma/cap/rss.xml', state: 'empty' },
  { id: 'tz-tma-en', name: 'United Republic of Tanzania: Tanzania Meteorological Authority', country: 'tz', country_name: 'United Republic of Tanzania', region: 1, feed: 'https://www.meteo.go.tz/cap/rss.xml', state: 'empty' },
  { id: 'ar-smn-es', name: 'Argentina: Servicio Meteorológico Nacional', country: 'ar', country_name: 'Argentina', region: 3, feed: 'https://ssl.smn.gob.ar/feeds/avisocorto_GeoRSS.xml', state: 'empty' },
  { id: 'cl-dmc-es', name: 'Chile: Dirección Meteorológica de Chile', country: 'cl', country_name: 'Chile', region: 3, feed: 'https://archivos.meteochile.gob.cl/portaldmc/meteochile/cap/rss.xml', state: 'starting' },
  { id: 'se-smhi-sv', name: 'Sweden: Swedish Meteorological and Hydrological Institute', country: 'se', country_name: 'Sweden', region: 6, feed: `${MA}sweden`, state: 'empty' },
];

/** The German alert: `Alerts.updateOne` replaces it with a CAP Update. */
export const UPDATED_SEED = 1021;

export const WORLD: WorldTemplate[] = [
  // ------------------------------------------------ MeteoAlarm, NUTS shapes
  { n: 1001, source: 'es-aemet-es', geom: 'nuts', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -2, onset: 1, expires: 20,
    polygons: [[[[-9.27, 42.9], [-8.8, 43.32], [-8.4, 43.37], [-8.25, 43.5], [-7.85, 43.7], [-7.69, 43.79], [-7.6, 43.67], [-7.04, 43.54], [-6.85, 43.4], [-6.83, 42.9], [-7.05, 42.5], [-7.6, 42.4], [-8.1, 42.75], [-8.65, 42.62], [-8.95, 42.58], [-9.1, 42.75]]]],
    infos: [
      { language: 'es-ES', event: 'Aviso naranja de fenómenos costeros',
        headline: 'Aviso naranja de fenómenos costeros en A Coruña y Lugo',
        description: 'Mar combinada del noroeste de 6 a 7 metros. Viento del oeste fuerza 7 en el litoral.',
        instruction: 'Evite acercarse a paseos marítimos, espigones y zonas expuestas al oleaje.',
        area: 'Costa de A Coruña; Costa de Lugo', sender: 'AEMET', web: 'https://www.aemet.es/es/eltiempo/prediccion/avisos' },
      { language: 'en-GB', event: 'Orange coastal event warning',
        headline: 'Orange coastal event warning for A Coruña and Lugo',
        description: 'Combined north-west swell of 6 to 7 metres. West winds force 7 along the coast.',
        instruction: 'Keep away from promenades, breakwaters and other places exposed to the waves.',
        area: 'A Coruña coast; Lugo coast', sender: 'AEMET', web: 'https://www.aemet.es/en/eltiempo/prediccion/avisos' },
    ] },
  { n: 1002, source: 'fr-meteofrance-fr', geom: 'nuts', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -3, onset: 2, expires: 18,
    polygons: [[[[-4.78, 48.4], [-4.6, 48.62], [-4, 48.72], [-3.55, 48.83], [-3, 48.8], [-2.55, 48.6], [-2.12, 48.62], [-2.2, 48.25], [-2.15, 47.7], [-2.45, 47.48], [-2.95, 47.53], [-3.35, 47.7], [-3.95, 47.85], [-4.37, 47.8], [-4.73, 48.04], [-4.4, 48.25]]]],
    infos: [
      { language: 'fr-FR', event: 'Vigilance jaune vent violent',
        headline: 'Vigilance jaune pour vent violent sur le Finistère, les Côtes-d\'Armor et le Morbihan',
        description: 'Rafales de 90 à 100 km/h sur les côtes, 80 km/h dans l\'intérieur des terres.',
        instruction: 'Limitez vos déplacements et rangez les objets susceptibles d\'être emportés.',
        area: 'Finistère; Côtes-d\'Armor; Morbihan', sender: 'Météo-France', web: 'https://vigilance.meteofrance.fr/' },
      { language: 'en-GB', event: 'Yellow wind warning',
        headline: 'Yellow wind warning for Finistère, Côtes-d\'Armor and Morbihan',
        description: 'Gusts of 90 to 100 km/h on the coast, 80 km/h inland.',
        instruction: 'Limit travel and secure objects that could be blown away.',
        area: 'Finistère; Côtes-d\'Armor; Morbihan', sender: 'Météo-France', web: 'https://vigilance.meteofrance.fr/' },
    ] },
  { n: 1003, source: 'pt-ipma-pt', geom: 'nuts', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -1.5, onset: 3, expires: 15,
    polygons: [[[[-8.87, 41.87], [-8.2, 42.15], [-7.95, 41.85], [-7.9, 41.45], [-8.1, 41.1], [-8.4, 40.85], [-8.65, 40.95], [-8.75, 41.4], [-8.82, 41.65]]]],
    infos: [
      { language: 'pt-PT', event: 'Aviso amarelo de precipitação',
        headline: 'Aviso amarelo de precipitação forte para o Alto Minho, Cávado, Ave e Porto',
        description: 'Períodos de chuva por vezes forte, que poderá ser acompanhada de trovoada.',
        area: 'Viana do Castelo; Braga; Porto', sender: 'IPMA', web: 'https://www.ipma.pt/pt/otempo/prev-sam/' },
      { language: 'en-GB', event: 'Yellow rain warning',
        headline: 'Yellow warning for heavy rain in Alto Minho, Cávado, Ave and Porto',
        description: 'Periods of rain, at times heavy, possibly with thunder.',
        area: 'Viana do Castelo; Braga; Porto', sender: 'IPMA', web: 'https://www.ipma.pt/en/otempo/prev-sam/' },
    ] },
  // The Innsbruck region is not named, so the union has a hole.
  { n: 1004, source: 'at-geosphere-de', geom: 'nuts', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -4, onset: 2, expires: 30,
    polygons: [
      [[[10.45, 47.58], [10.95, 47.48], [11.4, 47.48], [12.2, 47.7], [12.75, 47.68], [12.4, 47.3], [12.25, 47.08], [11.5, 47], [11, 46.77], [10.47, 46.85], [10.15, 47], [10.1, 47.3]],
       [[11.05, 47.12], [11.65, 47.12], [11.65, 47.33], [11.05, 47.33]]],
    ],
    infos: [
      { language: 'de-AT', event: 'Orange Schneewarnung',
        headline: 'Orange Warnung vor Schneefall in Außerfern, Tiroler Oberland und Tiroler Unterland',
        description: 'Oberhalb von 800 m 40 bis 60 cm Neuschnee, in Staulagen bis 80 cm. Schneebruch und Lawinengefahr.',
        instruction: 'Fahrten ins Gebirge nur mit Winterausrüstung. Lawinenwarndienst beachten.',
        area: 'Außerfern; Tiroler Oberland; Tiroler Unterland', sender: 'GeoSphere Austria', web: 'https://warnungen.zamg.at/' },
      { language: 'en-GB', event: 'Orange snow warning',
        headline: 'Orange warning for snow in Außerfern, Tiroler Oberland and Tiroler Unterland',
        description: 'Above 800 m 40 to 60 cm of new snow, up to 80 cm where the air is forced up the slopes. Trees may break under the snow; avalanche danger.',
        instruction: 'Drive into the mountains only with winter equipment. Follow the avalanche warning service.',
        area: 'Außerfern; Tiroler Oberland; Tiroler Unterland', sender: 'GeoSphere Austria', web: 'https://warnungen.zamg.at/' },
    ] },
  { n: 1005, source: 'pl-imgw-pl', geom: 'nuts', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -2.5, onset: 4, expires: 22,
    polygons: [[[[16.7, 54.58], [17.3, 54.75], [18.3, 54.83], [18.8, 54.6], [18.6, 54.35], [19.25, 54.35], [19.2, 54], [18.3, 53.75], [17.3, 53.95], [16.75, 54.2]]]],
    infos: [
      { language: 'pl-PL', event: 'Silny wiatr, ostrzeżenie 1. stopnia',
        headline: 'Ostrzeżenie meteorologiczne 1. stopnia przed silnym wiatrem dla województwa pomorskiego',
        description: 'Prognozuje się wiatr o średniej prędkości do 40 km/h, w porywach do 75 km/h, z kierunku zachodniego.',
        area: 'Słupski; Gdański; Trójmiejski', sender: 'IMGW-PIB', web: 'https://meteo.imgw.pl/' },
      { language: 'en-GB', event: 'Yellow wind warning',
        headline: 'Yellow warning for strong wind in Pomerania',
        description: 'West winds of up to 40 km/h, gusting to 75 km/h.',
        area: 'Słupski; Gdański; Trójmiejski', sender: 'IMGW-PIB', web: 'https://meteo.imgw.pl/' },
    ] },
  { n: 1006, source: 'ro-anm-ro', geom: 'nuts', severity: 'Severe', urgency: 'Immediate', certainty: 'Likely', category: 'met', sent: -0.5, onset: -0.5, expires: 9,
    polygons: [[[[25.55, 44.7], [26.4, 44.75], [27.2, 44.55], [27.35, 44.1], [26.4, 44], [25.65, 43.65], [25.4, 43.95], [25.5, 44.35]]]],
    infos: [
      { language: 'ro-RO', event: 'Cod portocaliu de ploi',
        headline: 'Cod portocaliu de ploi abundente în București, Ilfov, Giurgiu și Călărași',
        description: 'Cantitățile de apă vor depăși 40...50 l/mp și izolat 70...80 l/mp.',
        instruction: 'Evitați deplasările în zonele inundabile și nu traversați cursurile de apă.',
        area: 'București; Ilfov; Giurgiu; Călărași', sender: 'ANM', web: 'https://www.meteoromania.ro/avertizari/' },
      { language: 'en-GB', event: 'Orange rain warning',
        headline: 'Orange warning for heavy rain in Bucharest, Ilfov, Giurgiu and Călărași',
        description: 'Rainfall will exceed 40 to 50 l/m², locally 70 to 80 l/m².',
        instruction: 'Avoid travel in areas prone to flooding and do not cross watercourses.',
        area: 'București; Ilfov; Giurgiu; Călărași', sender: 'ANM', web: 'https://www.meteoromania.ro/avertizari/' },
    ] },
  // Attica and the island of Evia: two regions that do not touch.
  { n: 1007, source: 'gr-hnms-el', geom: 'nuts', severity: 'Extreme', urgency: 'Immediate', certainty: 'Observed', category: 'met', sent: -1, onset: -1, expires: 12,
    polygons: [
      [[[23.1, 37.95], [23.35, 38.2], [23.7, 38.3], [24.1, 38.2], [24.05, 37.85], [24, 37.65], [23.65, 37.9], [23.4, 37.95]]],
      [[[22.85, 38.85], [23.2, 39.05], [23.5, 38.85], [24.1, 38.65], [24.6, 38.15], [24.55, 37.95], [24.2, 38.15], [23.65, 38.4], [23.15, 38.65]]],
    ],
    infos: [
      { language: 'el-GR', event: 'Κόκκινη προειδοποίηση για βροχή',
        headline: 'Κόκκινη προειδοποίηση για ισχυρές βροχές και καταιγίδες στην Αττική και την Εύβοια',
        description: 'Ισχυρές βροχές και καταιγίδες, κατά τόπους πολύ ισχυρές, με χαλαζοπτώσεις και ισχυρούς κεραυνούς.',
        instruction: 'Αποφύγετε τις μετακινήσεις. Μην διασχίζετε χειμάρρους και πλημμυρισμένους δρόμους.',
        area: 'Αττική; Εύβοια', sender: 'ΕΜΥ', web: 'https://www.emy.gr/' },
      { language: 'en-GB', event: 'Red rain warning',
        headline: 'Red warning for heavy rain and thunderstorms in Attica and Evia',
        description: 'Heavy rain and thunderstorms, locally very heavy, with hail and frequent lightning.',
        instruction: 'Avoid all travel. Do not cross streams or flooded roads.',
        area: 'Attica; Evia', sender: 'HNMS', web: 'https://www.emy.gr/' },
    ] },
  { n: 1008, source: 'nl-knmi-nl', geom: 'nuts', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -1, onset: 5, expires: 16,
    polygons: [[[[4.75, 52.95], [5.2, 53.4], [6.2, 53.55], [7.2, 53.35], [7.2, 53.05], [6.4, 52.85], [5.8, 52.8], [5.4, 52.88], [5.1, 52.95]]]],
    infos: [
      { language: 'nl-NL', event: 'Code geel: windstoten',
        headline: 'Code geel voor zware windstoten in Friesland, Groningen en op de Waddeneilanden',
        description: 'Zware windstoten van 75 tot 90 km/u uit het westen.',
        area: 'Waddeneilanden; Friesland; Groningen', sender: 'KNMI', web: 'https://www.knmi.nl/nederland-nu/weer/waarschuwingen' },
      { language: 'en-GB', event: 'Yellow wind warning',
        headline: 'Yellow warning for severe gusts in Friesland, Groningen and the Wadden Islands',
        description: 'Severe westerly gusts of 75 to 90 km/h.',
        area: 'Wadden Islands; Friesland; Groningen', sender: 'KNMI', web: 'https://www.knmi.nl/nederland-nu/weer/waarschuwingen' },
    ] },
  { n: 1009, source: 'hr-dhmz-hr', geom: 'nuts', severity: 'Severe', urgency: 'Immediate', certainty: 'Likely', category: 'met', sent: -2, onset: -1, expires: 14,
    polygons: [[[[15.75, 43.9], [16.4, 43.95], [17.25, 43.45], [17.9, 43.05], [18.55, 42.6], [18.45, 42.45], [17.6, 42.75], [16.7, 42.9], [16.15, 43.3], [15.8, 43.55]]]],
    infos: [
      { language: 'hr-HR', event: 'Narančasto upozorenje: vjetar',
        headline: 'Narančasto upozorenje za olujnu buru u Dalmaciji',
        description: 'Olujna bura s udarima do 130 km/h, ponegdje i jačim. Mogući prekidi prometa preko mostova.',
        instruction: 'Izbjegavajte vožnju vozilima osjetljivim na bočni vjetar.',
        area: 'Šibensko-kninska; Splitsko-dalmatinska; Dubrovačko-neretvanska', sender: 'DHMZ', web: 'https://meteo.hr/upozorenja.php' },
      { language: 'en-GB', event: 'Orange wind warning',
        headline: 'Orange warning for storm-force bora in Dalmatia',
        description: 'Storm-force bora gusting to 130 km/h, locally more. Bridges may close to traffic.',
        instruction: 'Avoid driving high-sided vehicles.',
        area: 'Šibenik-Knin; Split-Dalmatia; Dubrovnik-Neretva', sender: 'DHMZ', web: 'https://meteo.hr/upozorenja.php' },
    ] },
  { n: 1010, source: 'cz-chmi-cs', geom: 'nuts', severity: 'Moderate', urgency: 'Future', certainty: 'Likely', category: 'met', sent: -3, onset: 10, expires: 34,
    polygons: [[[[14.6, 50.85], [15, 51], [15.55, 50.82], [15.75, 50.75], [16.4, 50.65], [16.55, 50.2], [16.3, 50.05], [15.4, 50.05], [15.05, 50.35], [14.6, 50.55]]]],
    infos: [
      { language: 'cs-CZ', event: 'Sněžení, nízký stupeň nebezpečí',
        headline: 'Výstraha před sněžením na horách Libereckého a Královéhradeckého kraje',
        description: 'Nad 800 m n. m. se očekává 10 až 20 cm nového sněhu.',
        area: 'Liberecký kraj; Královéhradecký kraj', sender: 'ČHMÚ', web: 'https://www.chmi.cz/' },
      { language: 'en-GB', event: 'Yellow snow warning',
        headline: 'Yellow warning for snow in the mountains of the Liberec and Hradec Králové regions',
        description: '10 to 20 cm of new snow expected above 800 m.',
        area: 'Liberec Region; Hradec Králové Region', sender: 'CHMI', web: 'https://www.chmi.cz/' },
    ] },

  // ------------------------------------------------- CAP polygons
  { n: 1011, source: 'ca-msc-xx', geom: 'cap', severity: 'Moderate', urgency: 'Future', certainty: 'Possible', category: 'met', sent: -1, onset: 6, expires: 15,
    polygons: [[[[-66.35, 44.25], [-65.8, 44.7], [-65.1, 45.05], [-64.5, 45.33], [-64.2, 45.1], [-64.35, 44.9], [-64.9, 44.62], [-65.5, 44.35], [-65.95, 44.1]]]],
    infos: [
      { language: 'en-CA', event: 'weather',
        headline: 'special weather statement in effect',
        description: 'Prolonged, heavy rainfall is possible.\n\nWhat: rainfall amounts of 50 to 75 mm.\n\nWhen: Saturday morning to Sunday afternoon.\n\nAdditional information: rain may be heavy at times on Saturday night.',
        area: 'Annapolis County; Digby County; Kings County', sender: 'Environment Canada', web: 'https://weather.gc.ca/' },
      { language: 'fr-CA', event: 'météo',
        headline: 'bulletin météorologique spécial en vigueur',
        description: 'Pluie abondante et prolongée possible.\n\nQuoi : quantités de pluie de 50 à 75 mm.\n\nQuand : de samedi matin à dimanche après-midi.\n\nRenseignements supplémentaires : la pluie pourrait être forte par moments samedi soir.',
        area: 'Comté d\'Annapolis; Comté de Digby; Comté de Kings', sender: 'Environnement Canada', web: 'https://meteo.gc.ca/' },
    ] },
  // French block first: the shown block is still the English one.
  { n: 1012, source: 'ca-msc-xx', geom: 'cap', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -2, onset: 3, expires: 21,
    polygons: [[[[-67.5, 49.9], [-66, 50.15], [-64.5, 50.25], [-64.5, 51.2], [-66.5, 51.3], [-67.8, 50.8]]]],
    infos: [
      { language: 'fr-CA', event: 'neige',
        headline: 'avertissement de neige en vigueur',
        description: 'Accumulations de neige de 20 à 30 cm prévues.\n\nQuand : de ce soir à dimanche matin.',
        instruction: 'Envisagez de reporter les déplacements non essentiels.',
        area: 'Sept-Îles - Port-Cartier; Baie-Trinité - Godbout', sender: 'Environnement Canada', web: 'https://meteo.gc.ca/' },
      { language: 'en-CA', event: 'snowfall',
        headline: 'snowfall warning in effect',
        description: 'Snowfall amounts of 20 to 30 cm are expected.\n\nWhen: tonight until Sunday morning.',
        instruction: 'Consider postponing non-essential travel.',
        area: 'Sept-Îles - Port-Cartier; Baie-Trinité - Godbout', sender: 'Environment Canada', web: 'https://weather.gc.ca/' },
    ] },
  { n: 1013, source: 'au-bom-en', geom: 'cap', severity: 'Severe', urgency: 'Immediate', certainty: 'Likely', category: 'met', sent: -0.4, onset: -0.4, expires: 2,
    polygons: [[[[152.6, -27.2], [153.2, -27.05], [153.55, -27.45], [153.55, -28.15], [152.95, -28.2], [152.55, -27.75]]]],
    infos: [
      { language: 'en-AU', event: 'Severe Thunderstorm Warning',
        headline: 'Severe thunderstorms with large hail and damaging winds for people in South East Queensland',
        description: 'Severe thunderstorms are likely to produce damaging winds, large hailstones and heavy rainfall that may lead to flash flooding over the next several hours. Locations which may be affected include Brisbane, Ipswich, Logan and the Gold Coast.',
        instruction: 'Move your car under cover or away from trees. Secure loose outdoor items. Stay indoors away from windows. Never walk, ride or drive through floodwater.',
        area: 'Brisbane; Ipswich; Logan; Gold Coast', sender: 'Australian Government Bureau of Meteorology, Queensland', web: 'https://www.bom.gov.au/qld/warnings/' },
    ] },
  { n: 1014, source: 'au-bom-en', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'fire', sent: -5, onset: 8, expires: 30,
    polygons: [[[[138.4, -33.2], [139.2, -33.2], [140.95, -33.7], [140.95, -34.6], [139.6, -35.1], [138.9, -35.5], [138.45, -35.2], [138.6, -34.5], [138.1, -33.8]]]],
    infos: [
      { language: 'en-AU', event: 'Fire Weather Warning',
        headline: 'Fire Weather Warning for the Mount Lofty Ranges, Mid North and Riverland fire districts',
        description: 'Hot, dry and gusty north-westerly winds ahead of a cool change are expected to produce Extreme Fire Danger ratings on Sunday.',
        instruction: 'Bush fire survival plans should be activated. Check the CFS website for total fire bans.',
        area: 'Mount Lofty Ranges; Mid North; Riverland', sender: 'Australian Government Bureau of Meteorology, South Australia', web: 'https://www.bom.gov.au/sa/warnings/' },
    ] },
  { n: 1015, source: 'au-bom-en', geom: 'cap', severity: 'Minor', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -6, onset: -6, expires: 40,
    polygons: [[[[148.8, -30.85], [149.7, -30.25], [150.2, -30.3], [150.1, -30.55], [149.5, -30.65], [148.9, -31.05]]]],
    infos: [
      { language: 'en-AU', event: 'Minor Flood Warning',
        headline: 'Minor flooding along the Namoi River at Narrabri and Wee Waa',
        description: 'The Namoi River at Narrabri is expected to exceed the minor flood level (6.70 m) on Saturday morning.',
        instruction: 'Do not drive into water of unknown depth and current.',
        area: 'Namoi River', sender: 'Australian Government Bureau of Meteorology, New South Wales', web: 'https://www.bom.gov.au/nsw/warnings/flood/namoi.shtml' },
      // The message's earlier step, kept in it but no longer in effect.
      { language: 'en-AU', event: 'Minor Flood Warning', past: true,
        headline: 'Minor flooding at Boggabri has eased',
        description: 'The Namoi River at Boggabri peaked below the minor flood level on Friday evening and is falling.',
        area: 'Namoi River at Boggabri', sender: 'Australian Government Bureau of Meteorology, New South Wales', web: 'https://www.bom.gov.au/nsw/warnings/flood/namoi.shtml' },
    ] },
  // Japanese first; the bare `en` block is the one shown.
  { n: 1016, source: 'jp-jma-ja', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -1, onset: 2, expires: 24,
    polygons: [[[[143.2, 44.6], [144.4, 44.2], [145.3, 44.3], [145.1, 43.8], [144.3, 43.6], [143.4, 43.7], [142.9, 44.1]]]],
    infos: [
      { language: 'ja-JP', event: '暴風警報',
        headline: '網走・北見・紋別地方では、７日昼過ぎまで暴風に警戒してください。',
        description: '発達した低気圧の影響で、網走・北見・紋別地方では、７日昼過ぎにかけて暴風となる見込みです。最大風速は陸上で２０メートル、海上で２５メートルの見込みです。',
        instruction: '不要不急の外出は控え、飛来物に注意してください。',
        area: '網走地方; 北見地方; 紋別地方', sender: '網走地方気象台', web: 'https://www.jma.go.jp/bosai/warning/' },
      { language: 'en', event: 'Storm Warning',
        headline: 'Storm warning for the Abashiri, Kitami and Monbetsu regions until Saturday afternoon',
        description: 'A deepening low will bring violent winds to the Abashiri, Kitami and Monbetsu regions until Saturday afternoon. Maximum sustained winds of 20 m/s over land and 25 m/s at sea.',
        instruction: 'Avoid going out unless necessary and watch for flying debris.',
        area: 'Abashiri; Kitami; Monbetsu', sender: 'Abashiri Local Meteorological Office', web: 'https://www.jma.go.jp/bosai/warning/' },
    ] },
  { n: 1017, source: 'ph-pagasa-en', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -1, onset: 3, expires: 18,
    polygons: [[[[122.9, 14.1], [123.6, 14.15], [124.45, 14.05], [124.45, 13.5], [124, 13], [123.7, 12.95], [123.2, 13.3], [122.9, 13.7]]]],
    infos: [
      { language: 'en-PH', event: 'Tropical Cyclone Wind Signal No. 3',
        headline: 'Typhoon "Ofel": Wind Signal No. 3 raised over Catanduanes, Camarines Sur and Albay',
        description: 'Storm-force winds of 89 to 117 km/h are expected within 18 hours. Moderate to significant threat to life and property. Storm surge of up to 2 metres is possible along the coasts.',
        instruction: 'Evacuate low-lying and coastal areas as directed by local authorities. Secure your home and stay indoors.',
        area: 'Catanduanes; Camarines Sur; Albay', sender: 'PAGASA Weather Division', web: 'https://bagong.pagasa.dost.gov.ph/tropical-cyclone/severe-weather-bulletin' },
    ] },
  // Arabic first, then English: right to left only when chosen.
  { n: 1018, source: 'ae-ncm-en', geom: 'cap', severity: 'Severe', urgency: 'Immediate', certainty: 'Observed', category: 'met', sent: -0.6, onset: -0.6, expires: 4,
    polygons: [[[[53.6, 24.15], [54.4, 24.5], [55.1, 25.05], [55.6, 25.4], [56, 25.2], [55.7, 24.6], [55, 24.1], [54, 23.6], [53.4, 23.75]]]],
    infos: [
      { language: 'ar-AE', event: 'ضباب',
        headline: 'ضباب كثيف مع انخفاض مدى الرؤية الأفقية إلى أقل من 500 متر على بعض المناطق الساحلية والداخلية',
        description: 'تكوّن ضباب كثيف على بعض المناطق الساحلية والداخلية مع انخفاض في مدى الرؤية الأفقية أحياناً حتى الساعة 10:00 صباحاً.',
        instruction: 'يرجى توخي الحذر أثناء القيادة والالتزام بالسرعات المحددة على اللوحات الإلكترونية.',
        area: 'أبوظبي؛ دبي؛ الشارقة', sender: 'المركز الوطني للأرصاد', web: 'https://www.ncm.gov.ae/' },
      { language: 'en-GB', event: 'Fog',
        headline: 'Dense fog with horizontal visibility below 500 m over some coastal and inland areas',
        description: 'Dense fog has formed over some coastal and internal areas, with horizontal visibility dropping at times until 10:00 AM.',
        instruction: 'Take care while driving and follow the speed limits shown on the electronic signs.',
        area: 'Abu Dhabi; Dubai; Sharjah', sender: 'National Center of Meteorology', web: 'https://www.ncm.gov.ae/' },
    ] },
  // Arabic only: shown right to left by default.
  { n: 1019, source: 'sa-ncm-ar', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -1.5, onset: 1, expires: 12,
    polygons: [[[[38.9, 22.5], [39.6, 22.6], [40.5, 21.8], [40.7, 21.1], [40.3, 20.7], [39.4, 21], [39, 21.6]]]],
    infos: [
      { language: 'ar-SA', event: 'أمطار رعدية غزيرة',
        headline: 'تنبيه أحمر: أمطار رعدية غزيرة على منطقة مكة المكرمة',
        description: 'أمطار غزيرة مصحوبة برياح نشطة وتساقط البرد وجريان السيول على محافظات جدة ومكة المكرمة والطائف.',
        instruction: 'الابتعاد عن مجاري السيول والأودية وعدم المجازفة بعبورها.',
        area: 'جدة؛ مكة المكرمة؛ الطائف', sender: 'المركز الوطني للأرصاد', web: 'https://ncm.gov.sa/' },
    ] },
  { n: 1020, source: 'no-metno-no', geom: 'cap', severity: 'Moderate', urgency: 'Future', certainty: 'Likely', category: 'met', sent: -2, onset: 9, expires: 33,
    polygons: [[[[4.8, 60.1], [5, 61], [5, 61.9], [6.3, 62.05], [7.1, 61.8], [7.6, 61.35], [7.7, 60.7], [7.1, 60.15], [6.3, 59.85], [5.4, 59.75]]]],
    infos: [
      { language: 'nb-NO', event: 'Kraftig regn',
        headline: 'Gult farevarsel for kraftig regn, Vestland',
        description: 'Lørdag ventes 60 til 90 millimeter nedbør på 24 timer, lokalt opptil 120 millimeter.',
        instruction: 'Sørg for at stikkrenner og bekker kan ta unna mye vann.',
        area: 'Vestland', sender: 'Meteorologisk institutt', web: 'https://www.yr.no/nb/farevarsler' },
      { language: 'en-GB', event: 'Heavy rain',
        headline: 'Yellow warning for heavy rain, Vestland',
        description: 'On Saturday 60 to 90 mm of rain is expected in 24 hours, locally up to 120 mm.',
        instruction: 'Make sure drains and culverts can carry large amounts of water.',
        area: 'Vestland', sender: 'Norwegian Meteorological Institute', web: 'https://www.yr.no/en/dangerwarnings' },
    ] },
  // German only, with the long compounds the panel must wrap. `updateOne`
  // replaces this one.
  { n: UPDATED_SEED, source: 'de-dwd-de', geom: 'cap', severity: 'Severe', urgency: 'Immediate', certainty: 'Likely', category: 'met', sent: -3, onset: -3, expires: 26,
    polygons: [[[[7.7, 48.1], [8.3, 48.15], [8.6, 47.95], [8.55, 47.65], [8.1, 47.62], [7.65, 47.62], [7.55, 47.85]]]],
    infos: [
      { language: 'de-DE', event: 'Unwetterwarnung vor extrem ergiebigem Dauerregen',
        headline: 'Amtliche Unwetterwarnung vor extrem ergiebigem Dauerregen im Dauerregenunwetterwarnungsgebietsabschnitt Hochschwarzwald-Südschwarzwald',
        description: 'Es tritt ergiebiger Dauerregen mit Unwetterpotenzial auf. Dabei werden Niederschlagsmengen zwischen 80 l/m² und 120 l/m², in Staulagen bis 150 l/m² erwartet.\n\nHochwasserrückhaltebeckenüberlaufwahrscheinlichkeit und Grundstücksentwässerungsanlagenüberlastungsgefahr sind erhöht.',
        instruction: 'ACHTUNG! Hinweis auf mögliche Gefahren: Verbreitet Überflutungen von Straßen und Kellern sowie Hochwasser an Bächen und kleineren Flüssen möglich. Meiden Sie Uferbereiche und Unterführungen.',
        area: 'Kreis Breisgau-Hochschwarzwald; Landkreis Waldshut; Kreis Lörrach; Schwarzwald-Baar-Kreis', sender: 'Deutscher Wetterdienst', web: 'https://www.dwd.de/DE/wetter/warnungen/warnWetter_node.html' },
    ] },
  // Four language blocks; the Italian one has no instruction of its own.
  { n: 1022, source: 'ch-meteoswiss-en', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -1.2, onset: 4, expires: 20,
    polygons: [[[[6.15, 46.35], [6.95, 46.95], [7.6, 47.45], [8.3, 47.5], [9.4, 47.55], [9.5, 47.3], [8.6, 47.05], [7.6, 46.85], [6.9, 46.5], [6.4, 46.3]]]],
    infos: [
      { language: 'de-CH', event: 'Sturmböen, Gefahrenstufe 3',
        headline: 'Erhebliche Gefahr: Sturmböen im Jura und im Mittelland',
        description: 'Böenspitzen von 90 bis 110 km/h im Flachland, auf den Jurahöhen 120 bis 140 km/h.',
        instruction: 'Wälder meiden. Lose Gegenstände sichern. Fenster und Türen schliessen.',
        area: 'Jura; westliches Mittelland; östliches Mittelland', sender: 'MeteoSchweiz', web: 'https://www.meteoschweiz.admin.ch/wetter/gefahren.html' },
      { language: 'fr-CH', event: 'Rafales de vent, degré de danger 3',
        headline: 'Danger marqué: rafales de vent sur le Jura et le Plateau',
        description: 'Pointes de 90 à 110 km/h en plaine, de 120 à 140 km/h sur les crêtes du Jura.',
        instruction: 'Éviter les forêts. Fixer les objets mobiles. Fermer portes et fenêtres.',
        area: 'Jura; Plateau occidental; Plateau oriental', sender: 'MétéoSuisse', web: 'https://www.meteosuisse.admin.ch/meteo/dangers.html' },
      { language: 'it-CH', event: 'Raffiche di vento, grado di pericolo 3',
        headline: 'Pericolo marcato: raffiche di vento sul Giura e sull\'Altopiano',
        description: 'Raffiche da 90 a 110 km/h in pianura, da 120 a 140 km/h sulle creste del Giura.',
        area: 'Giura; Altopiano occidentale; Altopiano orientale', sender: 'MeteoSvizzera', web: 'https://www.meteosvizzera.admin.ch/tempo/pericoli.html' },
      { language: 'en-GB', event: 'Wind gusts, danger level 3',
        headline: 'Considerable danger: wind gusts in the Jura and on the Swiss Plateau',
        description: 'Gusts of 90 to 110 km/h in the lowlands, 120 to 140 km/h on the Jura ridges.',
        instruction: 'Avoid forests. Secure loose objects. Close windows and doors.',
        area: 'Jura; western Swiss Plateau; eastern Swiss Plateau', sender: 'MeteoSwiss', web: 'https://www.meteoswiss.admin.ch/weather/hazards.html' },
    ] },
  // Only what CAP requires: no headline, instruction, sender or web, and
  // no urgency or certainty.
  { n: 1023, source: 'br-inmet-pt', geom: 'cap', severity: 'Moderate', urgency: null, certainty: null, category: 'met', sent: -2, onset: -1, expires: 22,
    polygons: [[[[-47.2, -23.1], [-46.2, -22.6], [-45, -22.7], [-44.9, -23.3], [-45.9, -23.8], [-46.9, -24], [-47.4, -23.6]]]],
    infos: [
      { language: 'pt-BR', event: 'Tempestade',
        description: 'Chuva entre 20 e 30 mm/h ou até 50 mm/dia, ventos intensos (40-60 km/h) e queda de granizo.',
        area: 'Metropolitana de São Paulo; Vale do Paraíba Paulista; Litoral Sul Paulista' },
    ] },
  // Vanua Levu, Taveuni and the Lau Group straddle 180: split there.
  { n: 1024, source: 'fj-fms-en', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -2, onset: 2, expires: 18,
    polygons: [[[[178.4, -16], [179.6, -15.8], [180.4, -16.2], [181.4, -17], [181.7, -18.3], [181.1, -19.2], [180.2, -18.9], [179.6, -17.6], [178.5, -17]]]],
    infos: [
      { language: 'en-FJ', event: 'Gale Warning',
        headline: 'Gale warning for Vanua Levu, Taveuni and the Lau Group',
        description: 'Tropical Cyclone Rae is expected to bring gale-force winds of 63 to 87 km/h with momentary gusts to 110 km/h, heavy rain and flooding of low-lying areas.',
        instruction: 'Secure loose items, stay away from the coast and listen to the radio for updates from the National Disaster Management Office.',
        area: 'Vanua Levu; Taveuni; Lau Group', sender: 'Fiji Meteorological Service', web: 'https://www.met.gov.fj/' },
    ] },
  { n: 1025, source: 'nz-metservice-en', geom: 'cap', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -4, onset: 1, expires: 23,
    polygons: [[[[168.7, -44], [169.5, -43.6], [170.7, -42.9], [171.3, -42.4], [171.6, -42.6], [170.9, -43.3], [169.9, -43.9], [169, -44.3]]]],
    infos: [
      { language: 'en-NZ', event: 'Heavy Rain Warning - Orange',
        headline: 'Heavy Rain Warning for Westland south of Otira',
        description: 'Expect 150 to 200 mm of rain about the ranges, 80 to 120 mm near the coast. Peak rates of 20 to 30 mm/h.',
        instruction: 'Streams and rivers may rise rapidly. Surface flooding and slips are possible.',
        area: 'Westland Ranges south of Otira', sender: 'MetService', web: 'https://www.metservice.com/warnings/home' },
    ] },
  { n: 1026, source: 'in-imd-en', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -3, onset: 2, expires: 27,
    polygons: [[[[79.6, 13.5], [80.35, 13.55], [80.3, 12.6], [79.9, 12.2], [79.4, 12.5], [79.4, 13.1]]]],
    infos: [
      { language: 'en-IN', event: 'Heavy to Very Heavy Rainfall',
        headline: 'Heavy to very heavy rainfall very likely at isolated places over Chennai, Tiruvallur, Kancheepuram and Chengalpattu',
        description: 'Northeast monsoon rainfall: 12 to 20 cm in 24 hours at isolated places. Waterlogging in low-lying urban areas.',
        instruction: 'Avoid waterlogged roads and stay indoors during intense spells.',
        area: 'Chennai; Tiruvallur; Kancheepuram; Chengalpattu', sender: 'Regional Meteorological Centre, Chennai', web: 'https://mausam.imd.gov.in/chennai/' },
    ] },
  // Spanish only.
  { n: 1027, source: 'mx-smn-es', geom: 'cap', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -2, onset: 1, expires: 30,
    polygons: [[[[-97.95, 22.5], [-97.4, 22.5], [-96.3, 19.6], [-95, 18.5], [-94.3, 18.1], [-94.5, 17.8], [-95.4, 18], [-96.7, 19.1], [-97.5, 20.8], [-98.1, 22]]]],
    infos: [
      { language: 'es-MX', event: 'Evento de Norte',
        headline: 'Evento de Norte con rachas de 70 a 90 km/h en el litoral de Tamaulipas y Veracruz',
        description: 'El frente frío número 9 originará evento de Norte con rachas de 70 a 90 km/h y oleaje de 2 a 3 metros de altura.',
        instruction: 'Atienda los avisos de Protección Civil y extreme precauciones en la navegación.',
        area: 'Sur de Tamaulipas; Veracruz', sender: 'Servicio Meteorológico Nacional', web: 'https://smn.conagua.gob.mx/' },
    ] },
  { n: 1028, source: 'za-saws-en', geom: 'cap', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -5, onset: 3, expires: 28,
    polygons: [[[[29.8, -29], [30.7, -28.4], [31.8, -28.5], [32.4, -28.7], [31.1, -29.9], [30.4, -30.7], [29.7, -30.3]]]],
    infos: [
      { language: 'en-ZA', event: 'Orange Level 6 Warning: Disruptive Rain',
        headline: 'Disruptive rain leading to flooding of roads and settlements over central and eastern KwaZulu-Natal',
        description: 'Heavy downpours of 50 to 80 mm are expected, leading to flooding of roads, settlements and low-lying bridges, and mudslides.',
        instruction: 'Do not cross rivers or swollen streams where water is above your ankles.',
        area: 'eThekwini; uMgungundlovu; iLembe; King Cetshwayo', sender: 'South African Weather Service', web: 'https://www.weathersa.co.za/home/warnings' },
    ] },
  { n: 1029, source: 'id-bmkg-id', geom: 'cap', severity: 'Moderate', urgency: 'Immediate', certainty: 'Likely', category: 'met', sent: -0.3, onset: -0.3, expires: 3,
    polygons: [[[[106.4, -6.1], [107.2, -6], [107.2, -6.7], [106.7, -6.8], [106.4, -6.5]]]],
    infos: [
      { language: 'id-ID', event: 'Hujan Lebat',
        headline: 'Hujan lebat disertai kilat/petir dan angin kencang di Jabodetabek',
        description: 'Berpotensi terjadi hujan dengan intensitas sedang hingga lebat disertai kilat/petir dan angin kencang di Jakarta, Bogor, Depok, Tangerang dan Bekasi.',
        area: 'DKI Jakarta; Bogor; Depok; Tangerang; Bekasi', sender: 'BMKG', web: 'https://nowcasting.bmkg.go.id/' },
      { language: 'en-US', event: 'Heavy Rain',
        headline: 'Heavy rain with lightning and strong winds in Greater Jakarta',
        description: 'Moderate to heavy rain with lightning and strong winds is expected in Jakarta, Bogor, Depok, Tangerang and Bekasi.',
        area: 'Jakarta; Bogor; Depok; Tangerang; Bekasi', sender: 'BMKG', web: 'https://nowcasting.bmkg.go.id/' },
    ] },
  { n: 1030, source: 'hk-hko-en', geom: 'cap', severity: 'Minor', urgency: 'Immediate', certainty: 'Likely', category: 'met', sent: -0.4, onset: -0.4, expires: 2,
    polygons: [[[[113.83, 22.2], [113.85, 22.5], [114.2, 22.56], [114.45, 22.45], [114.35, 22.2], [114.05, 22.15]]]],
    infos: [
      { language: 'en-HK', event: 'Thunderstorm Warning',
        headline: 'Thunderstorm Warning: isolated thunderstorms are likely over Hong Kong in the next couple of hours',
        description: 'Isolated thunderstorms are likely to occur over Hong Kong in the next couple of hours.',
        instruction: 'Stay indoors. Avoid open areas and do not touch metal objects outdoors.',
        area: 'Hong Kong', sender: 'Hong Kong Observatory', web: 'https://www.hko.gov.hk/en/wxinfo/warning/' },
      { language: 'zh-HK', event: '雷暴警告',
        headline: '雷暴警告：預料在未來一兩小時內，香港有局部地區性雷暴',
        description: '預料在未來一兩小時內，香港有局部地區性雷暴。',
        instruction: '留在室內，避免身處空曠地方，切勿觸摸戶外的金屬物體。',
        area: '香港', sender: '香港天文台', web: 'https://www.hko.gov.hk/tc/wxinfo/warning/' },
    ] },
  // Hebrew first, English second.
  { n: 1031, source: 'il-ims-he', geom: 'cap', severity: 'Moderate', urgency: 'Expected', certainty: 'Possible', category: 'met', sent: -2, onset: 4, expires: 20,
    polygons: [[[[34.9, 31.3], [35.45, 31.4], [35.55, 30.8], [35.1, 29.6], [34.9, 29.55], [34.5, 30.5]]]],
    infos: [
      { language: 'he-IL', event: 'שיטפונות',
        headline: 'חשש לשיטפונות בנחלי הנגב, הערבה וים המלח',
        description: 'צפויים גשמים מקומיים עזים מלווים בסופות רעמים, ועמם חשש לשיטפונות בנחלים.',
        instruction: 'הימנעו מטיולים בנחלים ומחציית מעברי מים.',
        area: 'הנגב; הערבה; ים המלח', sender: 'השירות המטאורולוגי', web: 'https://ims.gov.il/he/warnings' },
      { language: 'en-US', event: 'Flash floods',
        headline: 'Flash floods possible in the streams of the Negev, the Arava and the Dead Sea',
        description: 'Local heavy rain with thunderstorms is expected, with a risk of flash floods in the streams.',
        instruction: 'Avoid hiking in stream beds and do not cross flowing water.',
        area: 'Negev; Arava; Dead Sea', sender: 'Israel Meteorological Service', web: 'https://ims.gov.il/en/warnings' },
    ] },
  { n: 1032, source: 'is-imo-en', geom: 'cap', severity: 'Unknown', urgency: 'Expected', certainty: 'Observed', category: 'geo', sent: -7, onset: -9, expires: null,
    polygons: [[[[-22.75, 63.8], [-22, 64], [-21.6, 63.95], [-21.7, 63.8], [-22.4, 63.75]]]],
    infos: [
      { language: 'en-GB', event: 'Earthquake swarm',
        headline: 'Earthquake swarm on the Reykjanes peninsula has subsided',
        description: 'An earthquake swarm near Grindavík, with about 600 earthquakes, the largest M3.4, has subsided. Further activity cannot be ruled out.',
        area: 'Reykjanes peninsula', sender: 'Icelandic Meteorological Office', web: 'https://en.vedur.is/earthquakes-and-volcanism/' },
      { language: 'is-IS', event: 'Jarðskjálftahrina',
        headline: 'Jarðskjálftahrinu á Reykjanesskaga hefur slotað',
        description: 'Jarðskjálftahrinu við Grindavík, með um 600 skjálftum, þeim stærsta 3,4 að stærð, hefur slotað. Ekki er hægt að útiloka frekari virkni.',
        area: 'Reykjanesskagi', sender: 'Veðurstofa Íslands', web: 'https://www.vedur.is/skjalftar-og-eldgos/' },
    ] },
  { n: 1033, source: 'kr-kma-ko', geom: 'cap', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'env', sent: -1, onset: 2, expires: 14,
    polygons: [[[[126.5, 37.8], [127.4, 37.95], [127.6, 37.4], [127.2, 37], [126.6, 37.1]]]],
    infos: [
      { language: 'ko-KR', event: '황사주의보',
        headline: '서울·인천·경기도에 황사주의보',
        description: '황사로 인해 미세먼지(PM10) 농도가 시간당 평균 800㎍/㎥ 이상인 상태가 2시간 이상 지속될 것으로 예상됩니다.',
        instruction: '노약자와 호흡기 질환자는 실외 활동을 자제하십시오.',
        area: '서울; 인천; 경기도', sender: '기상청', web: 'https://www.weather.go.kr/w/special-report/list.do' },
      { language: 'en-US', event: 'Yellow Dust Advisory',
        headline: 'Yellow dust advisory for Seoul, Incheon and Gyeonggi-do',
        description: 'Yellow dust is expected to keep hourly PM10 concentrations above 800 µg/m³ for two hours or more.',
        instruction: 'Elderly people and those with respiratory conditions should limit outdoor activity.',
        area: 'Seoul; Incheon; Gyeonggi-do', sender: 'Korea Meteorological Administration', web: 'https://www.weather.go.kr/w/eng/' },
    ] },

  // ----------------------------------------- region codes only: no shape
  { n: 1034, source: 'it-meteoam-it', geom: 'none', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -2, onset: 1, expires: 19,
    anchor: [12.810059, 42.306769],
    polygons: [],
    infos: [
      { language: 'it-IT', event: 'Allerta gialla per temporali',
        headline: 'Allerta gialla per temporali su Lazio, Campania e Basilicata',
        description: 'Precipitazioni sparse, anche a carattere di rovescio o temporale, con quantitativi cumulati moderati.',
        area: 'Lazio; Campania; Basilicata', sender: 'Servizio Meteorologico dell\'Aeronautica Militare', web: 'https://www.meteoam.it/it/allerte' },
      { language: 'en-GB', event: 'Yellow Thunderstorm Warning',
        headline: 'Yellow thunderstorm warning for Lazio, Campania and Basilicata',
        description: 'Scattered showers and thunderstorms with moderate totals.',
        area: 'Lazio; Campania; Basilicata', sender: 'Italian Air Force Meteorological Service', web: 'https://www.meteoam.it/it/allerte' },
    ] },
  { n: UNSHAPED_SEED, source: 'ke-kmd-en', geom: 'none', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', category: 'met', sent: -6, onset: -2, expires: 46,
    anchor: [37.906193, 0.023559],
    polygons: [],
    infos: [
      { language: 'en-KE', event: 'Heavy Rainfall Alert',
        headline: 'Heavy rainfall expected over the Highlands East of the Rift Valley including Nairobi',
        description: 'Rainfall of more than 30 mm in 24 hours is expected, which may lead to flash floods in low-lying areas.',
        instruction: 'Residents are advised to avoid driving or walking through moving water and to stay away from riverbanks.',
        area: 'Nairobi; Kiambu; Murang\'a; Nyeri; Machakos', sender: 'Kenya Meteorological Department', web: 'https://meteo.go.ke/' },
    ] },
];
