// US airports for synthetic station reports, and the nearest-town names and
// time zones the mock NWS-style point forecast hands back.

import { wrapLon } from './geo.ts';

export interface Airport {
  icao: string;
  city: string;
  /** State or territory code. */
  state: string;
  lat: number;
  lon: number;
  tz: string;
}

const NY = 'America/New_York';
const CHI = 'America/Chicago';
const DEN = 'America/Denver';
const PHX = 'America/Phoenix';
const LA = 'America/Los_Angeles';
const DET = 'America/Detroit';
const IND = 'America/Indiana/Indianapolis';
const BOI = 'America/Boise';
const ANC = 'America/Anchorage';
const NOME = 'America/Nome';
const SITKA = 'America/Sitka';
const HNL = 'Pacific/Honolulu';
const SJU = 'America/Puerto_Rico';

const rows: [string, string, string, number, number, string][] = [
  ['KATL', 'Atlanta', 'GA', 33.6407, -84.4277, NY], ['KBOS', 'Boston', 'MA', 42.3656, -71.0096, NY], ['KORD', 'Chicago', 'IL', 41.9742, -87.9073, CHI],
  ['KDFW', 'Dallas-Fort Worth', 'TX', 32.8998, -97.0403, CHI], ['KDEN', 'Denver', 'CO', 39.8561, -104.6737, DEN], ['KLAX', 'Los Angeles', 'CA', 33.9416, -118.4085, LA],
  ['KSFO', 'San Francisco', 'CA', 37.6213, -122.379, LA], ['KSEA', 'Seattle', 'WA', 47.4502, -122.3088, LA], ['KPDX', 'Portland', 'OR', 45.5898, -122.5951, LA],
  ['KPHX', 'Phoenix', 'AZ', 33.4342, -112.0116, PHX], ['KLAS', 'Las Vegas', 'NV', 36.084, -115.1537, LA], ['KSLC', 'Salt Lake City', 'UT', 40.7899, -111.9791, DEN],
  ['KMSP', 'Minneapolis', 'MN', 44.8848, -93.2223, CHI], ['KMCI', 'Kansas City', 'MO', 39.2976, -94.7139, CHI], ['KMKC', 'Kansas City Downtown', 'MO', 39.1232, -94.5928, CHI],
  ['KSTL', 'St. Louis', 'MO', 38.7487, -90.37, CHI], ['KOMA', 'Omaha', 'NE', 41.3032, -95.8941, CHI], ['KDSM', 'Des Moines', 'IA', 41.534, -93.6631, CHI],
  ['KOKC', 'Oklahoma City', 'OK', 35.3931, -97.6007, CHI], ['KOUN', 'Norman', 'OK', 35.2456, -97.4721, CHI], ['KTUL', 'Tulsa', 'OK', 36.1984, -95.8881, CHI],
  ['KICT', 'Wichita', 'KS', 37.6499, -97.4331, CHI], ['KIAH', 'Houston', 'TX', 29.9902, -95.3368, CHI], ['KAUS', 'Austin', 'TX', 30.1975, -97.6664, CHI],
  ['KSAT', 'San Antonio', 'TX', 29.5337, -98.4698, CHI], ['KELP', 'El Paso', 'TX', 31.8072, -106.3776, DEN], ['KABQ', 'Albuquerque', 'NM', 35.0402, -106.609, DEN],
  ['KMSY', 'New Orleans', 'LA', 29.9934, -90.258, CHI], ['KMEM', 'Memphis', 'TN', 35.0424, -89.9767, CHI], ['KBNA', 'Nashville', 'TN', 36.1263, -86.6774, CHI],
  ['KLIT', 'Little Rock', 'AR', 34.7294, -92.2243, CHI], ['KJAN', 'Jackson', 'MS', 32.3112, -90.0759, CHI], ['KBHM', 'Birmingham', 'AL', 33.5629, -86.7535, CHI],
  ['KMIA', 'Miami', 'FL', 25.7959, -80.287, NY], ['KMCO', 'Orlando', 'FL', 28.4312, -81.3081, NY], ['KTPA', 'Tampa', 'FL', 27.9755, -82.5332, NY],
  ['KJAX', 'Jacksonville', 'FL', 30.4941, -81.6879, NY], ['KCLT', 'Charlotte', 'NC', 35.214, -80.9431, NY], ['KRDU', 'Raleigh-Durham', 'NC', 35.8801, -78.788, NY],
  ['KDCA', 'Washington', 'DC', 38.8512, -77.0402, NY], ['KBWI', 'Baltimore', 'MD', 39.1774, -76.6684, NY], ['KPHL', 'Philadelphia', 'PA', 39.8744, -75.2424, NY],
  ['KJFK', 'New York', 'NY', 40.6413, -73.7781, NY], ['KPIT', 'Pittsburgh', 'PA', 40.4915, -80.2329, NY], ['KCLE', 'Cleveland', 'OH', 41.4058, -81.8539, NY],
  ['KCMH', 'Columbus', 'OH', 39.998, -82.8919, NY], ['KCVG', 'Cincinnati', 'OH', 39.0489, -84.6678, NY], ['KDTW', 'Detroit', 'MI', 42.2162, -83.3554, DET],
  ['KGRR', 'Grand Rapids', 'MI', 42.8808, -85.5228, DET], ['KIND', 'Indianapolis', 'IN', 39.7173, -86.2944, IND], ['KMKE', 'Milwaukee', 'WI', 42.9472, -87.8966, CHI],
  ['KMSN', 'Madison', 'WI', 43.1399, -89.3375, CHI], ['KBUF', 'Buffalo', 'NY', 42.9405, -78.7322, NY], ['KALB', 'Albany', 'NY', 42.7483, -73.8017, NY],
  ['KSYR', 'Syracuse', 'NY', 43.1112, -76.1063, NY], ['KPWM', 'Portland', 'ME', 43.6462, -70.3093, NY], ['KBTV', 'Burlington', 'VT', 44.472, -73.1533, NY],
  ['KFSD', 'Sioux Falls', 'SD', 43.582, -96.7419, CHI], ['KRAP', 'Rapid City', 'SD', 44.0453, -103.0574, DEN], ['KBIS', 'Bismarck', 'ND', 46.7727, -100.746, CHI],
  ['KFAR', 'Fargo', 'ND', 46.9207, -96.8158, CHI], ['KDLH', 'Duluth', 'MN', 46.8421, -92.1936, CHI], ['KBIL', 'Billings', 'MT', 45.8077, -108.5429, DEN],
  ['KGTF', 'Great Falls', 'MT', 47.482, -111.3707, DEN], ['KBOI', 'Boise', 'ID', 43.5644, -116.2228, BOI], ['KGEG', 'Spokane', 'WA', 47.6199, -117.5338, LA],
  ['KRNO', 'Reno', 'NV', 39.4991, -119.7681, LA], ['KSAN', 'San Diego', 'CA', 32.7338, -117.1933, LA], ['KSMF', 'Sacramento', 'CA', 38.6951, -121.5908, LA],
  ['KFAT', 'Fresno', 'CA', 36.7762, -119.7181, LA], ['KMFR', 'Medford', 'OR', 42.3742, -122.8735, LA], ['KCYS', 'Cheyenne', 'WY', 41.1557, -104.8118, DEN],
  ['KCOS', 'Colorado Springs', 'CO', 38.8058, -104.7008, DEN], ['KAMA', 'Amarillo', 'TX', 35.2194, -101.7059, CHI], ['KLBB', 'Lubbock', 'TX', 33.6636, -101.8228, CHI],
  ['KMAF', 'Midland', 'TX', 31.9425, -102.2019, CHI], ['KCRP', 'Corpus Christi', 'TX', 27.7704, -97.5012, CHI], ['KBRO', 'Brownsville', 'TX', 25.9068, -97.4259, CHI],
  ['KSHV', 'Shreveport', 'LA', 32.4466, -93.8256, CHI], ['KLCH', 'Lake Charles', 'LA', 30.1261, -93.2234, CHI], ['KMOB', 'Mobile', 'AL', 30.6912, -88.2428, CHI],
  ['KTLH', 'Tallahassee', 'FL', 30.3965, -84.3503, NY], ['KSAV', 'Savannah', 'GA', 32.1276, -81.2021, NY], ['KCHS', 'Charleston', 'SC', 32.8986, -80.0405, NY],
  ['KORF', 'Norfolk', 'VA', 36.8946, -76.2012, NY], ['KRIC', 'Richmond', 'VA', 37.5052, -77.3197, NY], ['KSDF', 'Louisville', 'KY', 38.1744, -85.736, NY],
  ['KTYS', 'Knoxville', 'TN', 35.811, -83.994, NY], ['KSGF', 'Springfield', 'MO', 37.2457, -93.3886, CHI], ['KCOU', 'Columbia', 'MO', 38.8181, -92.2196, CHI],
  ['KTOP', 'Topeka', 'KS', 39.0686, -95.6225, CHI], ['KLNK', 'Lincoln', 'NE', 40.851, -96.7592, CHI], ['KGRI', 'Grand Island', 'NE', 40.9675, -98.3096, CHI],
  ['KDDC', 'Dodge City', 'KS', 37.7634, -99.9656, CHI], ['KGJT', 'Grand Junction', 'CO', 39.1224, -108.5267, DEN], ['KFLG', 'Flagstaff', 'AZ', 35.1385, -111.6712, PHX],
  ['KTUS', 'Tucson', 'AZ', 32.1161, -110.941, PHX],
  // Smaller fields, so the station layer is about as dense as the real one.
  ['KABE', 'Allentown', 'PA', 40.6521, -75.4408, NY], ['KACY', 'Atlantic City', 'NJ', 39.4576, -74.5772, NY], ['KAVL', 'Asheville', 'NC', 35.4362, -82.5418, NY],
  ['KBGR', 'Bangor', 'ME', 44.8074, -68.8281, NY], ['KBTR', 'Baton Rouge', 'LA', 30.5332, -91.1496, CHI], ['KCAE', 'Columbia', 'SC', 33.9388, -81.1195, NY],
  ['KCHA', 'Chattanooga', 'TN', 35.0353, -85.2038, NY], ['KCRW', 'Charleston', 'WV', 38.3731, -81.5932, NY], ['KDAY', 'Dayton', 'OH', 39.9024, -84.2194, NY],
  ['KEUG', 'Eugene', 'OR', 44.1246, -123.219, LA], ['KEVV', 'Evansville', 'IN', 38.037, -87.5324, CHI], ['KFWA', 'Fort Wayne', 'IN', 40.9785, -85.1951, IND],
  ['KGSO', 'Greensboro', 'NC', 36.0978, -79.9373, NY], ['KGSP', 'Greenville', 'SC', 34.8957, -82.2189, NY], ['KHSV', 'Huntsville', 'AL', 34.6372, -86.7751, CHI],
  ['KILM', 'Wilmington', 'NC', 34.2706, -77.9026, NY], ['KJAC', 'Jackson', 'WY', 43.6073, -110.7377, DEN], ['KLEX', 'Lexington', 'KY', 38.0365, -84.6059, NY],
  ['KMGM', 'Montgomery', 'AL', 32.3006, -86.394, CHI], ['KMHT', 'Manchester', 'NH', 42.9326, -71.4357, NY], ['KMLI', 'Moline', 'IL', 41.4485, -90.5075, CHI],
  ['KMSO', 'Missoula', 'MT', 46.9163, -114.0906, DEN], ['KPVD', 'Providence', 'RI', 41.724, -71.4282, NY], ['KBDL', 'Hartford', 'CT', 41.9389, -72.6832, NY],
  ['KROA', 'Roanoke', 'VA', 37.3255, -79.9754, NY], ['KROC', 'Rochester', 'NY', 43.1189, -77.6724, NY], ['KRST', 'Rochester', 'MN', 43.9083, -92.5, CHI],
  ['KSBN', 'South Bend', 'IN', 41.7087, -86.3173, IND], ['KSPI', 'Springfield', 'IL', 39.8441, -89.6779, CHI], ['KPIA', 'Peoria', 'IL', 40.6642, -89.6933, CHI],
  ['KTOL', 'Toledo', 'OH', 41.5868, -83.8078, NY], ['KLAN', 'Lansing', 'MI', 42.7787, -84.5874, DET], ['KMQT', 'Marquette', 'MI', 46.3536, -87.3954, DET],
  ['KGRB', 'Green Bay', 'WI', 44.4851, -88.1296, CHI], ['KLSE', 'La Crosse', 'WI', 43.879, -91.2567, CHI], ['KABR', 'Aberdeen', 'SD', 45.4491, -98.4218, CHI],
  ['KMOT', 'Minot', 'ND', 48.2594, -101.2803, CHI], ['KISN', 'Williston', 'ND', 48.1779, -103.6423, CHI], ['KGFK', 'Grand Forks', 'ND', 47.9493, -97.1761, CHI],
  ['KHLN', 'Helena', 'MT', 46.6068, -111.9827, DEN], ['KBZN', 'Bozeman', 'MT', 45.7775, -111.153, DEN], ['KIDA', 'Idaho Falls', 'ID', 43.5146, -112.0708, BOI],
  ['KPIH', 'Pocatello', 'ID', 42.9098, -112.5959, BOI], ['KTWF', 'Twin Falls', 'ID', 42.4818, -114.4877, BOI], ['KEKO', 'Elko', 'NV', 40.8249, -115.7917, LA],
  ['KWMC', 'Winnemucca', 'NV', 40.902, -117.8059, LA], ['KBFL', 'Bakersfield', 'CA', 35.4336, -119.0568, LA], ['KSBA', 'Santa Barbara', 'CA', 34.4262, -119.8404, LA],
  ['KMRY', 'Monterey', 'CA', 36.587, -121.8429, LA], ['KACV', 'Arcata', 'CA', 40.9781, -124.1086, LA], ['KRDD', 'Redding', 'CA', 40.509, -122.2934, LA],
  ['KOTH', 'North Bend', 'OR', 43.4171, -124.246, LA], ['KBLI', 'Bellingham', 'WA', 48.7928, -122.5375, LA], ['KYKM', 'Yakima', 'WA', 46.5682, -120.544, LA],
  ['KPSC', 'Pasco', 'WA', 46.2647, -119.119, LA], ['KRDM', 'Redmond', 'OR', 44.2541, -121.15, LA], ['KLMT', 'Klamath Falls', 'OR', 42.1561, -121.7332, LA],
  ['KCPR', 'Casper', 'WY', 42.908, -106.4644, DEN], ['KRKS', 'Rock Springs', 'WY', 41.5942, -109.0652, DEN], ['KSHR', 'Sheridan', 'WY', 44.7692, -106.9803, DEN],
  ['KLAR', 'Laramie', 'WY', 41.3121, -105.675, DEN], ['KALS', 'Alamosa', 'CO', 37.4349, -105.8667, DEN], ['KPUB', 'Pueblo', 'CO', 38.2891, -104.4966, DEN],
  ['KDRO', 'Durango', 'CO', 37.1515, -107.7538, DEN], ['KFMN', 'Farmington', 'NM', 36.7412, -108.2299, DEN], ['KROW', 'Roswell', 'NM', 33.3016, -104.5306, DEN],
  ['KLRU', 'Las Cruces', 'NM', 32.2894, -106.922, DEN], ['KPRC', 'Prescott', 'AZ', 34.6545, -112.4196, PHX], ['KIPL', 'Imperial', 'CA', 32.8342, -115.5787, LA],
  ['KNYL', 'Yuma', 'AZ', 32.6566, -114.606, PHX], ['KSJT', 'San Angelo', 'TX', 31.3577, -100.4963, CHI], ['KABI', 'Abilene', 'TX', 32.4113, -99.6819, CHI],
  ['KACT', 'Waco', 'TX', 31.6113, -97.2305, CHI], ['KTYR', 'Tyler', 'TX', 32.3541, -95.4024, CHI], ['KLRD', 'Laredo', 'TX', 27.5438, -99.4616, CHI],
  ['KHRL', 'Harlingen', 'TX', 26.2285, -97.6544, CHI], ['KVCT', 'Victoria', 'TX', 28.8526, -96.9185, CHI], ['KGLS', 'Galveston', 'TX', 29.2653, -94.8604, CHI],
  ['KBPT', 'Beaumont', 'TX', 29.9508, -94.0207, CHI], ['KLFT', 'Lafayette', 'LA', 30.2053, -91.9876, CHI], ['KMLU', 'Monroe', 'LA', 32.5109, -92.0377, CHI],
  ['KGPT', 'Gulfport', 'MS', 30.4073, -89.0701, CHI], ['KPNS', 'Pensacola', 'FL', 30.4734, -87.1866, CHI], ['KECP', 'Panama City', 'FL', 30.3571, -85.7955, CHI],
  ['KGNV', 'Gainesville', 'FL', 29.69, -82.2718, NY], ['KDAB', 'Daytona Beach', 'FL', 29.1799, -81.0581, NY], ['KPBI', 'West Palm Beach', 'FL', 26.6832, -80.0956, NY],
  ['KRSW', 'Fort Myers', 'FL', 26.5362, -81.7552, NY], ['KEYW', 'Key West', 'FL', 24.5561, -81.7596, NY], ['KMYR', 'Myrtle Beach', 'SC', 33.6797, -78.9283, NY],
  ['KAGS', 'Augusta', 'GA', 33.3699, -81.9645, NY], ['KMCN', 'Macon', 'GA', 32.6928, -83.6492, NY], ['KCSG', 'Columbus', 'GA', 32.5163, -84.9389, NY],
  ['KABY', 'Albany', 'GA', 31.5355, -84.1945, NY], ['KTRI', 'Tri-Cities', 'TN', 36.4752, -82.4074, NY], ['KLYH', 'Lynchburg', 'VA', 37.3267, -79.2004, NY],
  ['KSBY', 'Salisbury', 'MD', 38.3405, -75.5103, NY], ['KACK', 'Nantucket', 'MA', 41.2531, -70.0602, NY], ['KHYA', 'Hyannis', 'MA', 41.6693, -70.2804, NY],
  ['KCAR', 'Caribou', 'ME', 46.8715, -68.0179, NY], ['KBHB', 'Bar Harbor', 'ME', 44.4497, -68.3616, NY], ['KART', 'Watertown', 'NY', 43.9919, -76.0217, NY],
  ['KELM', 'Elmira', 'NY', 42.1599, -76.8916, NY], ['KERI', 'Erie', 'PA', 42.0831, -80.1739, NY], ['KAVP', 'Wilkes-Barre', 'PA', 41.3385, -75.7234, NY],
  ['KIPT', 'Williamsport', 'PA', 41.2418, -76.9211, NY], ['KMDT', 'Harrisburg', 'PA', 40.1935, -76.7634, NY], ['KCKB', 'Clarksburg', 'WV', 39.2966, -80.2281, NY],
  ['KHTS', 'Huntington', 'WV', 38.3667, -82.558, NY], ['KPAH', 'Paducah', 'KY', 37.0608, -88.7738, CHI], ['KJLN', 'Joplin', 'MO', 37.1518, -94.4983, CHI],
  ['KFSM', 'Fort Smith', 'AR', 35.3366, -94.3674, CHI], ['KXNA', 'Fayetteville', 'AR', 36.2819, -94.3068, CHI], ['KTXK', 'Texarkana', 'AR', 33.4537, -93.991, CHI],
  ['KLAW', 'Lawton', 'OK', 34.5677, -98.4166, CHI], ['KGCK', 'Garden City', 'KS', 37.9275, -100.7244, CHI], ['KHYS', 'Hays', 'KS', 38.8422, -99.2732, CHI],
  ['KSLN', 'Salina', 'KS', 38.791, -97.6522, CHI], ['KSTJ', 'St. Joseph', 'MO', 39.772, -94.9097, CHI], ['KCID', 'Cedar Rapids', 'IA', 41.8847, -91.7108, CHI],
  ['KDBQ', 'Dubuque', 'IA', 42.402, -90.7095, CHI], ['KSUX', 'Sioux City', 'IA', 42.4026, -96.3844, CHI], ['KALO', 'Waterloo', 'IA', 42.5571, -92.4003, CHI],
  ['KLBF', 'North Platte', 'NE', 41.1262, -100.6837, CHI], ['KBFF', 'Scottsbluff', 'NE', 41.874, -103.5956, DEN], ['KVTN', 'Valentine', 'NE', 42.8578, -100.5483, CHI],
  ['KPIR', 'Pierre', 'SD', 44.3827, -100.286, CHI], ['KHON', 'Huron', 'SD', 44.3852, -98.2285, CHI], ['KINL', 'International Falls', 'MN', 48.5662, -93.4031, CHI],
  ['KBJI', 'Bemidji', 'MN', 47.5094, -94.9337, CHI], ['KSTC', 'St. Cloud', 'MN', 45.5466, -94.0599, CHI], ['KRHI', 'Rhinelander', 'WI', 45.6312, -89.4675, CHI],
  ['KAUW', 'Wausau', 'WI', 44.9262, -89.627, CHI], ['KEAU', 'Eau Claire', 'WI', 44.8658, -91.4843, CHI], ['KAPN', 'Alpena', 'MI', 45.0781, -83.5603, DET],
  ['KTVC', 'Traverse City', 'MI', 44.7414, -85.5822, DET], ['KHTL', 'Houghton Lake', 'MI', 44.3598, -84.6711, DET], ['KGGW', 'Glasgow', 'MT', 48.2125, -106.6148, DEN],
  ['KMLS', 'Miles City', 'MT', 46.428, -105.8864, DEN], ['KHVR', 'Havre', 'MT', 48.543, -109.7623, DEN], ['KPUW', 'Pullman', 'WA', 46.7439, -117.1096, LA],
  ['KALW', 'Walla Walla', 'WA', 46.0949, -118.288, LA], ['KPDT', 'Pendleton', 'OR', 45.6951, -118.8414, LA], ['KBNO', 'Burns', 'OR', 43.5919, -118.955, LA],
  ['KTPH', 'Tonopah', 'NV', 38.0602, -117.0872, LA], ['KELY', 'Ely', 'NV', 39.2997, -114.8419, LA], ['KCDC', 'Cedar City', 'UT', 37.701, -113.0988, DEN],
  ['KVEL', 'Vernal', 'UT', 40.4409, -109.5099, DEN], ['KCNY', 'Moab', 'UT', 38.755, -109.7548, DEN], ['KPGA', 'Page', 'AZ', 36.9261, -111.4484, PHX],
  ['KINW', 'Winslow', 'AZ', 35.0219, -110.7225, PHX], ['KGUP', 'Gallup', 'NM', 35.5111, -108.7893, DEN], ['KTCC', 'Tucumcari', 'NM', 35.1828, -103.6032, DEN],
  ['KCVN', 'Clovis', 'NM', 34.4251, -103.0793, DEN], ['KDHT', 'Dalhart', 'TX', 36.0226, -102.5473, CHI], ['KCDS', 'Childress', 'TX', 34.4338, -100.288, CHI],
  ['KSPS', 'Wichita Falls', 'TX', 33.9888, -98.4919, CHI], ['KDRT', 'Del Rio', 'TX', 29.3742, -100.9272, CHI], ['KALI', 'Alice', 'TX', 27.7409, -98.0269, CHI],
  ['KFST', 'Fort Stockton', 'TX', 30.9157, -102.9161, CHI], ['KPRX', 'Paris', 'TX', 33.6366, -95.4508, CHI], ['KHOT', 'Hot Springs', 'AR', 34.478, -93.0962, CHI],
  ['KJBR', 'Jonesboro', 'AR', 35.8317, -90.6464, CHI], ['KTUP', 'Tupelo', 'MS', 34.2681, -88.7699, CHI], ['KMEI', 'Meridian', 'MS', 32.3326, -88.7519, CHI],
  ['KDHN', 'Dothan', 'AL', 31.3213, -85.4496, CHI], ['KVLD', 'Valdosta', 'GA', 30.7825, -83.2767, NY], ['KBQK', 'Brunswick', 'GA', 31.2588, -81.4665, NY],
  ['KFLO', 'Florence', 'SC', 34.1854, -79.7239, NY], ['KEWN', 'New Bern', 'NC', 35.073, -77.0429, NY], ['KHSE', 'Cape Hatteras', 'NC', 35.2328, -75.6222, NY],
  ['KDOV', 'Dover', 'DE', 39.1295, -75.466, NY], ['KISP', 'Islip', 'NY', 40.7952, -73.1002, NY], ['KEWR', 'Newark', 'NJ', 40.6925, -74.1687, NY],
  ['KHPN', 'White Plains', 'NY', 41.067, -73.7076, NY], ['KORH', 'Worcester', 'MA', 42.2673, -71.8757, NY], ['KCON', 'Concord', 'NH', 43.2027, -71.5023, NY],
  ['KMPV', 'Montpelier', 'VT', 44.2035, -72.5623, NY], ['KMSS', 'Massena', 'NY', 44.9358, -74.8456, NY], ['KGFL', 'Glens Falls', 'NY', 43.3412, -73.6103, NY],
  ['KBGM', 'Binghamton', 'NY', 42.2087, -75.9798, NY], ['KJST', 'Johnstown', 'PA', 40.3161, -78.8339, NY], ['KUNV', 'State College', 'PA', 40.8493, -77.8487, NY],
  ['KMGW', 'Morgantown', 'WV', 39.6429, -79.9163, NY], ['KBKW', 'Beckley', 'WV', 37.7873, -81.1242, NY], ['KCHO', 'Charlottesville', 'VA', 38.1386, -78.4529, NY],
  ['KIAD', 'Washington Dulles', 'VA', 38.9445, -77.4558, NY], ['KLOZ', 'London', 'KY', 37.0872, -84.077, NY], ['KBWG', 'Bowling Green', 'KY', 36.9645, -86.4197, CHI],
  ['KCKV', 'Clarksville', 'TN', 36.6219, -87.415, CHI], ['KMKL', 'Jackson', 'TN', 35.5999, -88.9156, CHI], ['KCGI', 'Cape Girardeau', 'MO', 37.2253, -89.5708, CHI],
  ['KDEC', 'Decatur', 'IL', 39.8346, -88.8657, CHI], ['KCMI', 'Champaign', 'IL', 40.0392, -88.2781, CHI], ['KRFD', 'Rockford', 'IL', 42.1954, -89.0972, CHI],
  ['KLAF', 'Lafayette', 'IN', 40.4123, -86.9369, IND], ['KBMG', 'Bloomington', 'IN', 39.146, -86.6168, IND], ['KMFD', 'Mansfield', 'OH', 40.8214, -82.5166, NY],
  ['KCAK', 'Akron', 'OH', 40.9161, -81.4422, NY], ['KYNG', 'Youngstown', 'OH', 41.2607, -80.6791, NY], ['KZZV', 'Zanesville', 'OH', 39.9444, -81.8921, NY],
  ['KFNT', 'Flint', 'MI', 42.9655, -83.7436, DET], ['KMBS', 'Saginaw', 'MI', 43.5329, -84.0796, DET], ['KAZO', 'Kalamazoo', 'MI', 42.2349, -85.5521, DET],
  ['KMKG', 'Muskegon', 'MI', 43.1695, -86.2382, DET], ['KESC', 'Escanaba', 'MI', 45.7227, -87.0937, DET], ['KCMX', 'Hancock', 'MI', 47.1684, -88.4891, DET],
  ['KANJ', 'Sault Ste. Marie', 'MI', 46.4792, -84.3684, DET], ['KOSH', 'Oshkosh', 'WI', 43.9844, -88.557, CHI], ['KHIB', 'Hibbing', 'MN', 47.3866, -92.839, CHI],
  ['KBRD', 'Brainerd', 'MN', 46.3983, -94.1381, CHI], ['KMKT', 'Mankato', 'MN', 44.2216, -93.9187, CHI], ['KTVF', 'Thief River Falls', 'MN', 48.0656, -96.185, CHI],
  ['KDVL', 'Devils Lake', 'ND', 48.1142, -98.9088, CHI], ['KJMS', 'Jamestown', 'ND', 46.9297, -98.6782, CHI], ['KDIK', 'Dickinson', 'ND', 46.7974, -102.802, DEN],
  ['KMBG', 'Mobridge', 'SD', 45.5465, -100.408, CHI], ['KMHE', 'Mitchell', 'SD', 43.7748, -98.0386, CHI], ['KOFK', 'Norfolk', 'NE', 41.9855, -97.4351, CHI],
  ['KEAR', 'Kearney', 'NE', 40.727, -99.0068, CHI], ['KMCK', 'McCook', 'NE', 40.2063, -100.5921, CHI], ['KGLD', 'Goodland', 'KS', 39.3707, -101.699, DEN],
  ['KEMP', 'Emporia', 'KS', 38.3321, -96.1912, CHI], ['KCNU', 'Chanute', 'KS', 37.6688, -95.4851, CHI], ['KPNC', 'Ponca City', 'OK', 36.732, -97.0998, CHI],
  ['KGAG', 'Gage', 'OK', 36.2955, -99.7764, CHI], ['KGUY', 'Guymon', 'OK', 36.6851, -101.508, CHI], ['KMLC', 'McAlester', 'OK', 34.8824, -95.7835, CHI],
  ['KADM', 'Ardmore', 'OK', 34.303, -97.0196, CHI], ['KHRO', 'Harrison', 'AR', 36.2615, -93.1547, CHI], ['KVIH', 'Rolla', 'MO', 38.1274, -91.7695, CHI],
  ['KIRK', 'Kirksville', 'MO', 40.0935, -92.5449, CHI], ['KOTM', 'Ottumwa', 'IA', 41.1066, -92.4479, CHI], ['KBRL', 'Burlington', 'IA', 40.7832, -91.1255, CHI],
  ['KSPW', 'Spencer', 'IA', 43.1655, -95.2028, CHI], ['KFOD', 'Fort Dodge', 'IA', 42.5515, -94.1926, CHI], ['KCDR', 'Chadron', 'NE', 42.8376, -103.0954, DEN],
  ['KRIW', 'Riverton', 'WY', 43.0642, -108.4598, DEN], ['KCOD', 'Cody', 'WY', 44.5202, -109.0238, DEN], ['KWRL', 'Worland', 'WY', 43.9657, -107.9508, DEN],
  ['KEVW', 'Evanston', 'WY', 41.2748, -111.035, DEN], ['KGCC', 'Gillette', 'WY', 44.3489, -105.5394, DEN], ['KLWT', 'Lewistown', 'MT', 47.0493, -109.4666, DEN],
  ['KCTB', 'Cut Bank', 'MT', 48.6084, -112.3761, DEN], ['KGPI', 'Kalispell', 'MT', 48.3105, -114.256, DEN], ['KBTM', 'Butte', 'MT', 45.9548, -112.4975, DEN],
  ['KSMN', 'Salmon', 'ID', 45.1238, -113.8811, BOI], ['KLWS', 'Lewiston', 'ID', 46.3745, -117.0154, LA], ['KMYL', 'McCall', 'ID', 44.8897, -116.1013, BOI],
  ['KBKE', 'Baker City', 'OR', 44.8373, -117.8091, LA], ['KLKV', 'Lakeview', 'OR', 42.161, -120.399, LA], ['KSLE', 'Salem', 'OR', 44.9095, -123.0026, LA],
  ['KAST', 'Astoria', 'OR', 46.158, -123.8787, LA], ['KHQM', 'Hoquiam', 'WA', 46.9712, -123.9366, LA], ['KUIL', 'Quillayute', 'WA', 47.9375, -124.5628, LA],
  ['KOLM', 'Olympia', 'WA', 46.9694, -122.9025, LA], ['KEAT', 'Wenatchee', 'WA', 47.3988, -120.2068, LA], ['KOMK', 'Omak', 'WA', 48.4644, -119.5181, LA],
  ['KCEC', 'Crescent City', 'CA', 41.7802, -124.2365, LA], ['KUKI', 'Ukiah', 'CA', 39.126, -123.2009, LA], ['KSTS', 'Santa Rosa', 'CA', 38.509, -122.8128, LA],
  ['KSCK', 'Stockton', 'CA', 37.8942, -121.2386, LA], ['KBIH', 'Bishop', 'CA', 37.3731, -118.3636, LA], ['KPSP', 'Palm Springs', 'CA', 33.8297, -116.5067, LA],
  ['KDAG', 'Daggett', 'CA', 34.8537, -116.7868, LA], ['KSMX', 'Santa Maria', 'CA', 34.8989, -120.4575, LA], ['KPRB', 'Paso Robles', 'CA', 35.6729, -120.6271, LA],
  ['KTRK', 'Truckee', 'CA', 39.32, -120.1396, LA], ['KLOL', 'Lovelock', 'NV', 40.0664, -118.565, LA],
  // Alaska (the Aleutians reach across the antimeridian), Hawaii and the
  // territories the NWS forecasts for.
  ['PANC', 'Anchorage', 'AK', 61.1744, -149.9964, ANC], ['PAFA', 'Fairbanks', 'AK', 64.8151, -147.8561, ANC], ['PAJN', 'Juneau', 'AK', 58.3547, -134.5763, 'America/Juneau'],
  ['PAKN', 'King Salmon', 'AK', 58.6768, -156.6492, ANC], ['PADQ', 'Kodiak', 'AK', 57.75, -152.4939, ANC], ['PAOM', 'Nome', 'AK', 64.5122, -165.4453, NOME],
  ['PABR', 'Utqiagvik', 'AK', 71.2854, -156.766, ANC], ['PABE', 'Bethel', 'AK', 60.7798, -161.838, ANC], ['PAKT', 'Ketchikan', 'AK', 55.3556, -131.7137, SITKA],
  ['PASI', 'Sitka', 'AK', 57.0471, -135.3616, SITKA], ['PAYA', 'Yakutat', 'AK', 59.5033, -139.66, 'America/Yakutat'], ['PAOT', 'Kotzebue', 'AK', 66.8847, -162.5985, NOME],
  ['PADU', 'Unalaska', 'AK', 53.8998, -166.5435, NOME], ['PACD', 'Cold Bay', 'AK', 55.2056, -162.7254, NOME], ['PASN', 'St. Paul Island', 'AK', 57.1673, -170.2204, NOME],
  ['PADK', 'Adak Island', 'AK', 51.878, -176.646, 'America/Adak'], ['PASY', 'Shemya', 'AK', 52.7123, 174.1137, 'America/Adak'], ['PHNL', 'Honolulu', 'HI', 21.3187, -157.9225, HNL],
  ['PHOG', 'Kahului', 'HI', 20.8986, -156.4305, HNL], ['PHTO', 'Hilo', 'HI', 19.7214, -155.0485, HNL], ['PHKO', 'Kona', 'HI', 19.7388, -156.0456, HNL],
  ['PHLI', 'Lihue', 'HI', 21.976, -159.339, HNL], ['TJSJ', 'San Juan', 'PR', 18.4394, -66.0018, SJU], ['TJPS', 'Ponce', 'PR', 18.0083, -66.563, SJU],
  ['TJBQ', 'Aguadilla', 'PR', 18.495, -67.1294, SJU], ['TIST', 'St. Thomas', 'VI', 18.3373, -64.9734, 'America/St_Thomas'],
  ['TISX', 'St. Croix', 'VI', 17.7019, -64.7986, 'America/St_Thomas'], ['PGUM', 'Guam', 'GU', 13.4834, 144.796, 'Pacific/Guam'],
  ['PGSN', 'Saipan', 'MP', 15.119, 145.729, 'Pacific/Saipan'], ['NSTU', 'Pago Pago', 'AS', -14.331, -170.7105, 'Pacific/Pago_Pago'],
];

export const AIRPORTS: Airport[] = rows.map(([icao, city, state, lat, lon, tz]) => ({ icao, city, state, lat, lon, tz }));

/** The nearest airport, measuring the short way round the antimeridian. */
export function nearestAirport(lon: number, lat: number): { a: Airport; km: number } {
  let best = AIRPORTS[0];
  let bestD = Infinity;
  const k = Math.cos((lat * Math.PI) / 180);
  for (const a of AIRPORTS) {
    const d = Math.hypot(wrapLon(a.lon - lon) * k, a.lat - lat) * 111.32;
    if (d < bestD) {
      bestD = d;
      best = a;
    }
  }
  return { a: best, km: bestD };
}

/** The synthetic storm that verifies georeferencing sits on downtown Kansas
 *  City, Missouri (the city's OSM node), so it can be checked by eye against
 *  the basemap's "Kansas City" label. */
export const GEOREF_CITY = { name: 'Kansas City', lat: 39.0997, lon: -94.5786 };
