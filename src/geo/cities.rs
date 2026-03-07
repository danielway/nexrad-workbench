//! Built-in major US cities for map overlay.

use super::layer::{GeoFeature, GeoLayer, GeoLayerType};
use geo_types::Coord;

/// Population tier for controlling visibility at different zoom levels.
#[derive(Clone, Copy)]
#[allow(dead_code)] // Tier info used for future zoom-based filtering
enum Tier {
    /// Major metro (pop > 500k) — visible at all zoom levels
    Major,
    /// Medium city (pop 200k-500k) — visible at zoom >= 1.5
    Medium,
    /// Smaller city (pop 100k-200k) — visible at zoom >= 3.0
    Small,
}

#[allow(dead_code)] // Tier field reserved for zoom-based filtering
struct CityEntry {
    name: &'static str,
    lat: f64,
    lon: f64,
    tier: Tier,
}

/// Built-in list of major US cities.
static CITIES: &[CityEntry] = &[
    // Major metros
    CityEntry { name: "New York", lat: 40.7128, lon: -74.0060, tier: Tier::Major },
    CityEntry { name: "Los Angeles", lat: 34.0522, lon: -118.2437, tier: Tier::Major },
    CityEntry { name: "Chicago", lat: 41.8781, lon: -87.6298, tier: Tier::Major },
    CityEntry { name: "Houston", lat: 29.7604, lon: -95.3698, tier: Tier::Major },
    CityEntry { name: "Phoenix", lat: 33.4484, lon: -112.0740, tier: Tier::Major },
    CityEntry { name: "Philadelphia", lat: 39.9526, lon: -75.1652, tier: Tier::Major },
    CityEntry { name: "San Antonio", lat: 29.4241, lon: -98.4936, tier: Tier::Major },
    CityEntry { name: "San Diego", lat: 32.7157, lon: -117.1611, tier: Tier::Major },
    CityEntry { name: "Dallas", lat: 32.7767, lon: -96.7970, tier: Tier::Major },
    CityEntry { name: "Austin", lat: 30.2672, lon: -97.7431, tier: Tier::Major },
    CityEntry { name: "Jacksonville", lat: 30.3322, lon: -81.6557, tier: Tier::Major },
    CityEntry { name: "Fort Worth", lat: 32.7555, lon: -97.3308, tier: Tier::Major },
    CityEntry { name: "Columbus", lat: 39.9612, lon: -82.9988, tier: Tier::Major },
    CityEntry { name: "Charlotte", lat: 35.2271, lon: -80.8431, tier: Tier::Major },
    CityEntry { name: "Indianapolis", lat: 39.7684, lon: -86.1581, tier: Tier::Major },
    CityEntry { name: "San Francisco", lat: 37.7749, lon: -122.4194, tier: Tier::Major },
    CityEntry { name: "Seattle", lat: 47.6062, lon: -122.3321, tier: Tier::Major },
    CityEntry { name: "Denver", lat: 39.7392, lon: -104.9903, tier: Tier::Major },
    CityEntry { name: "Nashville", lat: 36.1627, lon: -86.7816, tier: Tier::Major },
    CityEntry { name: "Oklahoma City", lat: 35.4676, lon: -97.5164, tier: Tier::Major },
    CityEntry { name: "Washington DC", lat: 38.9072, lon: -77.0369, tier: Tier::Major },
    CityEntry { name: "El Paso", lat: 31.7619, lon: -106.4850, tier: Tier::Major },
    CityEntry { name: "Boston", lat: 42.3601, lon: -71.0589, tier: Tier::Major },
    CityEntry { name: "Memphis", lat: 35.1495, lon: -90.0490, tier: Tier::Major },
    CityEntry { name: "Louisville", lat: 38.2527, lon: -85.7585, tier: Tier::Major },
    CityEntry { name: "Portland", lat: 45.5152, lon: -122.6784, tier: Tier::Major },
    CityEntry { name: "Las Vegas", lat: 36.1699, lon: -115.1398, tier: Tier::Major },
    CityEntry { name: "Milwaukee", lat: 43.0389, lon: -87.9065, tier: Tier::Major },
    CityEntry { name: "Albuquerque", lat: 35.0844, lon: -106.6504, tier: Tier::Major },
    CityEntry { name: "Detroit", lat: 42.3314, lon: -83.0458, tier: Tier::Major },
    CityEntry { name: "Atlanta", lat: 33.7490, lon: -84.3880, tier: Tier::Major },
    CityEntry { name: "Miami", lat: 25.7617, lon: -80.1918, tier: Tier::Major },
    CityEntry { name: "Minneapolis", lat: 44.9778, lon: -93.2650, tier: Tier::Major },
    CityEntry { name: "Kansas City", lat: 39.0997, lon: -94.5786, tier: Tier::Major },
    CityEntry { name: "New Orleans", lat: 29.9511, lon: -90.0715, tier: Tier::Major },
    CityEntry { name: "St. Louis", lat: 38.6270, lon: -90.1994, tier: Tier::Major },
    CityEntry { name: "Salt Lake City", lat: 40.7608, lon: -111.8910, tier: Tier::Major },
    CityEntry { name: "Tampa", lat: 27.9506, lon: -82.4572, tier: Tier::Major },
    CityEntry { name: "Pittsburgh", lat: 40.4406, lon: -79.9959, tier: Tier::Major },
    CityEntry { name: "Cincinnati", lat: 39.1031, lon: -84.5120, tier: Tier::Major },
    CityEntry { name: "Orlando", lat: 28.5383, lon: -81.3792, tier: Tier::Major },
    CityEntry { name: "Cleveland", lat: 41.4993, lon: -81.6944, tier: Tier::Major },
    // Medium cities
    CityEntry { name: "Raleigh", lat: 35.7796, lon: -78.6382, tier: Tier::Medium },
    CityEntry { name: "Omaha", lat: 41.2565, lon: -95.9345, tier: Tier::Medium },
    CityEntry { name: "Tucson", lat: 32.2226, lon: -110.9747, tier: Tier::Medium },
    CityEntry { name: "Tulsa", lat: 36.1540, lon: -95.9928, tier: Tier::Medium },
    CityEntry { name: "Wichita", lat: 37.6872, lon: -97.3301, tier: Tier::Medium },
    CityEntry { name: "Arlington", lat: 32.7357, lon: -97.1081, tier: Tier::Medium },
    CityEntry { name: "Bakersfield", lat: 35.3733, lon: -119.0187, tier: Tier::Medium },
    CityEntry { name: "Aurora", lat: 39.7294, lon: -104.8319, tier: Tier::Medium },
    CityEntry { name: "Honolulu", lat: 21.3069, lon: -157.8583, tier: Tier::Medium },
    CityEntry { name: "Anchorage", lat: 61.2181, lon: -149.9003, tier: Tier::Medium },
    CityEntry { name: "Lexington", lat: 38.0406, lon: -84.5037, tier: Tier::Medium },
    CityEntry { name: "St. Paul", lat: 44.9537, lon: -93.0900, tier: Tier::Medium },
    CityEntry { name: "Buffalo", lat: 42.8864, lon: -78.8784, tier: Tier::Medium },
    CityEntry { name: "Richmond", lat: 37.5407, lon: -77.4360, tier: Tier::Medium },
    CityEntry { name: "Boise", lat: 43.6150, lon: -116.2023, tier: Tier::Medium },
    CityEntry { name: "Des Moines", lat: 41.5868, lon: -93.6250, tier: Tier::Medium },
    CityEntry { name: "Birmingham", lat: 33.5207, lon: -86.8025, tier: Tier::Medium },
    CityEntry { name: "Rochester", lat: 43.1566, lon: -77.6088, tier: Tier::Medium },
    CityEntry { name: "Baton Rouge", lat: 30.4515, lon: -91.1871, tier: Tier::Medium },
    CityEntry { name: "Little Rock", lat: 34.7465, lon: -92.2896, tier: Tier::Medium },
    CityEntry { name: "Madison", lat: 43.0731, lon: -89.4012, tier: Tier::Medium },
    CityEntry { name: "Spokane", lat: 47.6588, lon: -117.4260, tier: Tier::Medium },
    CityEntry { name: "Jackson", lat: 32.2988, lon: -90.1848, tier: Tier::Medium },
    CityEntry { name: "Knoxville", lat: 35.9606, lon: -83.9207, tier: Tier::Medium },
    CityEntry { name: "Mobile", lat: 30.6954, lon: -88.0399, tier: Tier::Medium },
    CityEntry { name: "Shreveport", lat: 32.5252, lon: -93.7502, tier: Tier::Medium },
    CityEntry { name: "Chattanooga", lat: 35.0456, lon: -85.3097, tier: Tier::Medium },
    CityEntry { name: "Fargo", lat: 46.8772, lon: -96.7898, tier: Tier::Medium },
    CityEntry { name: "Sioux Falls", lat: 43.5446, lon: -96.7311, tier: Tier::Medium },
    CityEntry { name: "Billings", lat: 45.7833, lon: -108.5007, tier: Tier::Medium },
    CityEntry { name: "Rapid City", lat: 44.0805, lon: -103.2310, tier: Tier::Medium },
    // Smaller cities — visible only at higher zoom
    CityEntry { name: "Springfield IL", lat: 39.7817, lon: -89.6501, tier: Tier::Small },
    CityEntry { name: "Springfield MO", lat: 37.2090, lon: -93.2923, tier: Tier::Small },
    CityEntry { name: "Topeka", lat: 39.0473, lon: -95.6752, tier: Tier::Small },
    CityEntry { name: "Lincoln", lat: 40.8136, lon: -96.7026, tier: Tier::Small },
    CityEntry { name: "Lubbock", lat: 33.5779, lon: -101.8552, tier: Tier::Small },
    CityEntry { name: "Amarillo", lat: 35.2220, lon: -101.8313, tier: Tier::Small },
    CityEntry { name: "Laredo", lat: 27.5036, lon: -99.5076, tier: Tier::Small },
    CityEntry { name: "Corpus Christi", lat: 27.8006, lon: -97.3964, tier: Tier::Small },
    CityEntry { name: "Savannah", lat: 32.0809, lon: -81.0912, tier: Tier::Small },
    CityEntry { name: "Charleston SC", lat: 32.7765, lon: -79.9311, tier: Tier::Small },
    CityEntry { name: "Charleston WV", lat: 38.3498, lon: -81.6326, tier: Tier::Small },
    CityEntry { name: "Columbia SC", lat: 34.0007, lon: -81.0348, tier: Tier::Small },
    CityEntry { name: "Tallahassee", lat: 30.4383, lon: -84.2807, tier: Tier::Small },
    CityEntry { name: "Pensacola", lat: 30.4213, lon: -87.2169, tier: Tier::Small },
    CityEntry { name: "Huntsville", lat: 34.7304, lon: -86.5861, tier: Tier::Small },
    CityEntry { name: "Montgomery", lat: 32.3792, lon: -86.3077, tier: Tier::Small },
    CityEntry { name: "Augusta", lat: 33.4735, lon: -81.9748, tier: Tier::Small },
    CityEntry { name: "Greenville SC", lat: 34.8526, lon: -82.3940, tier: Tier::Small },
    CityEntry { name: "Dayton", lat: 39.7589, lon: -84.1916, tier: Tier::Small },
    CityEntry { name: "Akron", lat: 41.0814, lon: -81.5190, tier: Tier::Small },
    CityEntry { name: "Syracuse", lat: 43.0481, lon: -76.1474, tier: Tier::Small },
    CityEntry { name: "Hartford", lat: 41.7658, lon: -72.6734, tier: Tier::Small },
    CityEntry { name: "Providence", lat: 41.8240, lon: -71.4128, tier: Tier::Small },
    CityEntry { name: "Duluth", lat: 46.7867, lon: -92.1005, tier: Tier::Small },
    CityEntry { name: "Green Bay", lat: 44.5133, lon: -88.0133, tier: Tier::Small },
    CityEntry { name: "Cedar Rapids", lat: 41.9779, lon: -91.6656, tier: Tier::Small },
    CityEntry { name: "Davenport", lat: 41.5236, lon: -90.5776, tier: Tier::Small },
    CityEntry { name: "Peoria", lat: 40.6936, lon: -89.5890, tier: Tier::Small },
    CityEntry { name: "Evansville", lat: 37.9716, lon: -87.5711, tier: Tier::Small },
    CityEntry { name: "Fort Wayne", lat: 41.0793, lon: -85.1394, tier: Tier::Small },
    CityEntry { name: "Grand Rapids", lat: 42.9634, lon: -85.6681, tier: Tier::Small },
    CityEntry { name: "Lansing", lat: 42.7325, lon: -84.5555, tier: Tier::Small },
    CityEntry { name: "Missoula", lat: 46.8721, lon: -114.0001, tier: Tier::Small },
    CityEntry { name: "Great Falls", lat: 47.5002, lon: -111.3008, tier: Tier::Small },
    CityEntry { name: "Cheyenne", lat: 41.1400, lon: -104.8202, tier: Tier::Small },
    CityEntry { name: "Casper", lat: 42.8501, lon: -106.3252, tier: Tier::Small },
    CityEntry { name: "Helena", lat: 46.5958, lon: -112.0270, tier: Tier::Small },
    CityEntry { name: "Bismarck", lat: 46.8083, lon: -100.7837, tier: Tier::Small },
    CityEntry { name: "Pierre", lat: 44.3683, lon: -100.3510, tier: Tier::Small },
    CityEntry { name: "Reno", lat: 39.5296, lon: -119.8138, tier: Tier::Small },
    CityEntry { name: "Santa Fe", lat: 35.6870, lon: -105.9378, tier: Tier::Small },
    CityEntry { name: "Flagstaff", lat: 35.1983, lon: -111.6513, tier: Tier::Small },
    CityEntry { name: "Colorado Springs", lat: 38.8339, lon: -104.8214, tier: Tier::Small },
    CityEntry { name: "Pueblo", lat: 38.2544, lon: -104.6091, tier: Tier::Small },
    CityEntry { name: "Waco", lat: 31.5493, lon: -97.1467, tier: Tier::Small },
    CityEntry { name: "Abilene", lat: 32.4487, lon: -99.7331, tier: Tier::Small },
    CityEntry { name: "Midland", lat: 31.9973, lon: -102.0779, tier: Tier::Small },
    CityEntry { name: "McAllen", lat: 26.2034, lon: -98.2300, tier: Tier::Small },
    CityEntry { name: "Brownsville", lat: 25.9017, lon: -97.4975, tier: Tier::Small },
    CityEntry { name: "Lake Charles", lat: 30.2266, lon: -93.2174, tier: Tier::Small },
    CityEntry { name: "Monroe", lat: 32.5093, lon: -92.1193, tier: Tier::Small },
    CityEntry { name: "Hattiesburg", lat: 31.3271, lon: -89.2903, tier: Tier::Small },
    CityEntry { name: "Paducah", lat: 37.0834, lon: -88.6001, tier: Tier::Small },
    CityEntry { name: "Bowling Green", lat: 36.9685, lon: -86.4808, tier: Tier::Small },
    CityEntry { name: "Joplin", lat: 37.0842, lon: -94.5133, tier: Tier::Small },
    CityEntry { name: "Dodge City", lat: 37.7528, lon: -100.0171, tier: Tier::Small },
    CityEntry { name: "Norfolk", lat: 36.8508, lon: -76.2859, tier: Tier::Small },
    CityEntry { name: "Wilmington NC", lat: 34.2257, lon: -77.9447, tier: Tier::Small },
    CityEntry { name: "Albany", lat: 42.6526, lon: -73.7562, tier: Tier::Small },
    CityEntry { name: "Portland ME", lat: 43.6591, lon: -70.2568, tier: Tier::Small },
    CityEntry { name: "Burlington", lat: 44.4759, lon: -73.2121, tier: Tier::Small },
    CityEntry { name: "Concord", lat: 43.2081, lon: -71.5376, tier: Tier::Small },
];

/// Build the cities layer from the built-in data.
pub fn build_cities_layer() -> GeoLayer {
    let mut layer = GeoLayer::new(GeoLayerType::Cities);

    for city in CITIES {
        layer.features.push(GeoFeature::Point(
            Coord {
                x: city.lon,
                y: city.lat,
            },
            Some(city.name.to_string()),
        ));
    }

    layer
}

