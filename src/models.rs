use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Marker {
    pub url: String,
    pub latitude: f64,
    pub longitude: f64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub x_offset: Option<i32>,
    pub y_offset: Option<i32>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Polygon {
    pub fill_color: Option<String>,
    pub stroke_color: Option<String>,
    pub stroke_width: Option<u32>,
    pub path: Vec<(f64, f64)>, // (lat, lon) pairs
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Circle {
    pub fill_color: Option<String>,
    pub stroke_color: Option<String>,
    pub stroke_width: Option<u32>,
    pub radius: f64,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct StaticMapRequest {
    pub style: String,
    pub latitude: f64,
    pub longitude: f64,
    pub zoom: f64,
    pub width: u32,
    pub height: u32,
    pub scale: Option<u32>,
    pub format: Option<String>,
    pub bearing: Option<f64>,
    pub pitch: Option<f64>,
    pub markers: Option<Vec<Marker>>,
    pub polygons: Option<Vec<Polygon>>,
    pub circles: Option<Vec<Circle>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct GridMap {
    pub direction: String,
    pub map: StaticMapRequest,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct GridRow {
    pub direction: String,
    pub maps: Vec<GridMap>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MultiStaticMapRequest {
    pub grid: Vec<GridRow>,
}
