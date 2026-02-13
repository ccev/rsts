use axum::{
    extract::{Query, Json, Path, State},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
    middleware::{self, Next},
    http::Request,
};
use tower_http::catch_panic::CatchPanicLayer;
use image::{ColorType, DynamicImage, ImageEncoder};
use maplibre_native::ImageRendererBuilder;
use std::collections::HashMap;
use std::io::{Cursor, Write, Read};
use std::num::NonZeroU32;
use tokio::net::TcpListener;
use url::Url;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, error, debug};

mod models;
mod utils;
mod template_engine;
mod config;
mod tile_cache;

use models::{StaticMapRequest, MultiStaticMapRequest};
use utils::fetch_image;
use template_engine::TemplateEngine;
use config::Config;
use tile_cache::TileCache;

#[derive(Deserialize)]
struct ContextWrapper(serde_json::Value);

impl From<ContextWrapper> for tera::Context {
    fn from(wrapper: ContextWrapper) -> Self {
        tera::Context::from_value(wrapper.0).unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize)]
struct WorkerPayload {
    request: StaticMapRequest,
    style_json: serde_json::Value,
    marker_data: HashMap<String, Vec<u8>>,
}

struct AppState {
    template_engine: TemplateEngine,
    config: Config,
    http_client: reqwest::Client,
    tile_cache: TileCache,
    render_semaphore: Arc<tokio::sync::Semaphore>,
}

fn main() -> anyhow::Result<()> {
    // Check for worker mode first
    if std::env::args().any(|arg| arg == "--worker") {
        return worker_main();
    }

    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    std::panic::set_hook(Box::new(|panic_info| {
        let location = panic_info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_else(|| "unknown".to_string());
        let payload = panic_info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            *s
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.as_str()
        } else {
            "unknown panic payload"
        };
        error!("PANIC at {}: {}", location, message);
    }));

    let config = Config::load()?;
    let thread_count = config.thread_count.unwrap_or_else(|| num_cpus::get());

    info!("starting server with {} threads", thread_count);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(thread_count)
        .enable_all()
        .build()?;

    runtime.block_on(async_main(config))
}

fn worker_main() -> anyhow::Result<()> {
    let mut input_bytes = Vec::new();
    std::io::stdin().read_to_end(&mut input_bytes)?;
    let payload: WorkerPayload = serde_json::from_slice(&input_bytes)?;
    
    let mut marker_images = HashMap::new();
    for (url, data) in payload.marker_data {
        let img = image::load_from_memory(&data)?;
        marker_images.insert(url, img);
    }

    match render_map_internal(&payload.request, marker_images, payload.style_json) {
        Ok(img) => {
            let mut cursor = Cursor::new(Vec::new());
            let (w, h) = (img.width(), img.height());
            let encoder = image::codecs::png::PngEncoder::new(&mut cursor);
            let img_rgba = img.to_rgba8();
            encoder.write_image(&img_rgba, w, h, ColorType::Rgba8.into())?;
            std::io::stdout().write_all(&cursor.into_inner())?;
            Ok(())
        },
        Err(e) => {
            eprintln!("Worker error: {}", e);
            std::process::exit(1);
        }
    }
}

async fn log_request_response(
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().to_string();
    info!("incoming request: {} {}", method, path);
    let response = next.run(req).await;
    info!("request result: {} for {} {}", response.status(), method, path);
    response
}

async fn async_main(config: Config) -> anyhow::Result<()> {
    let template_engine = TemplateEngine::new("data/templates".into())?;
    let http_client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let cache_dir = std::path::Path::new("data/cache").to_path_buf();
    let max_cache_gb = config.cache_size_gb.unwrap_or(10);
    let tile_cache = TileCache::new(cache_dir, max_cache_gb).await?;

    // Allow concurrency up to CPU count since we are using isolated processes
    let concurrency_limit = num_cpus::get().max(4); 
    let render_semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency_limit));

    let state = Arc::new(AppState { 
        template_engine,
        config,
        http_client,
        tile_cache,
        render_semaphore,
    });

    let app = Router::new()
        .route("/staticmap", get(get_static_map).post(post_static_map))
        .route("/staticmap/{template}", get(get_static_map_template).post(post_static_map_template))
        .route("/multistaticmap", post(post_multi_static_map))
        .route("/multistaticmap/{template}", get(get_multi_static_map_template).post(post_multi_static_map_template))
        .route("/tiles/{id}/{z}/{x}/{y}", get(proxy_tile))
        .layer(middleware::from_fn(log_request_response))
        .layer(CatchPanicLayer::new())
        .with_state(state);

    let addr = "0.0.0.0:3001";
    info!("listening on {}", addr);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// --- Handlers ---

async fn proxy_tile(
    State(state): State<Arc<AppState>>,
    Path((id, z, x, y)): Path<(String, u32, u32, u32)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let source_url_template = match state.config.tile_sources.get(&id) {
        Some(url) => url,
        None => return (axum::http::StatusCode::NOT_FOUND, "tile source not found").into_response(),
    };

    let scale = params.get("scale").cloned().unwrap_or_else(|| "1".to_string());
    let cache_key = format!("{}:{}/{}/{}/{}", id, z, x, y, scale);

    if let Ok(Some(bytes)) = state.tile_cache.get(&cache_key).await {
        return ([("content-type", "image/png")], bytes).into_response();
    }

    let url = source_url_template
        .replace("{z}", &z.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string())
        .replace("{scale}", &scale);

    match state.http_client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status();
            let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("image/png").to_string();
            let bytes = resp.bytes().await.unwrap_or_default();
            if status.is_success() {
                let _ = state.tile_cache.insert(&cache_key, bytes.to_vec()).await;
                (
                    axum::http::StatusCode::OK,
                    [("content-type", content_type)],
                    bytes
                ).into_response()
            } else {
                debug!("tile source {} returned error: {}", id, status);
                (axum::http::StatusCode::NOT_FOUND, "tile not found").into_response()
            }
        },
        Err(e) => {
            error!("proxy error for {}: {}", id, e);
            (axum::http::StatusCode::NOT_FOUND, "tile source error").into_response()
        },
    }
}

async fn get_static_map(State(state): State<Arc<AppState>>, Query(params): Query<StaticMapRequest>) -> Response {
    match generate_static_map_image(state, &params).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("static map error: {}", e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error generating map: {}", e)).into_response()
        },
    }
}

async fn post_static_map(State(state): State<Arc<AppState>>, Json(params): Json<StaticMapRequest>) -> Response {
    match generate_static_map_image(state, &params).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("static map error: {}", e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error generating map: {}", e)).into_response()
        },
    }
}

async fn post_multi_static_map(State(state): State<Arc<AppState>>, Json(params): Json<MultiStaticMapRequest>) -> Response {
    match generate_multi_map_image(state, &params).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("multi-static map error: {}", e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error generating multi-map: {}", e)).into_response()
        },
    }
}

async fn get_static_map_template(
    State(state): State<Arc<AppState>>,
    Path(template): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let context = parse_query_params(params);
    match render_and_generate_static(state, &template, &context).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("template static map error ({}): {}", template, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error rendering template: {}", e)).into_response()
        },
    }
}

async fn post_static_map_template(
    State(state): State<Arc<AppState>>,
    Path(template): Path<String>,
    Json(context): Json<ContextWrapper>,
) -> Response {
    match render_and_generate_static(state, &template, &context.into()).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("template static map error ({}): {}", template, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error rendering template: {}", e)).into_response()
        },
    }
}

async fn get_multi_static_map_template(
    State(state): State<Arc<AppState>>,
    Path(template): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let context = parse_query_params(params);
    match render_and_generate_multi(state, &template, &context).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("template multi map error ({}): {}", template, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error rendering template: {}", e)).into_response()
        },
    }
}

async fn post_multi_static_map_template(
    State(state): State<Arc<AppState>>,
    Path(template): Path<String>,
    Json(context): Json<ContextWrapper>,
) -> Response {
    match render_and_generate_multi(state, &template, &context.into()).await {
        Ok(img) => encode_image(img),
        Err(e) => {
            error!("template multi map error ({}): {}", template, e);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("error rendering template: {}", e)).into_response()
        },
    }
}

fn encode_image(img: DynamicImage) -> Response {
    let mut cursor = Cursor::new(Vec::new());
    let (w, h) = (img.width(), img.height());
    let encoder = image::codecs::png::PngEncoder::new(&mut cursor);
    let img_rgba = img.to_rgba8();
    if let Err(e) = encoder.write_image(&img_rgba, w, h, ColorType::Rgba8.into()) {
        error!("encoding error: {}", e);
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "image encoding error").into_response();
    }
    let bytes = cursor.into_inner();
    ([("content-type", "image/png")], bytes).into_response()
}

// --- Helper Functions ---

fn parse_query_params(params: Vec<(String, String)>) -> tera::Context {
    let mut context = tera::Context::new();
    let mut map: HashMap<String, serde_json::Value> = HashMap::new();
    for (key, value) in params {
        let (clean_key, is_json) = if key.ends_with(".json") || key.ends_with("_json") {
            (key.trim_end_matches(".json").trim_end_matches("_json").to_string(), true)
        } else { (key, false) };
        let parsed_value = if is_json {
            serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
        } else {
            if let Ok(num) = value.parse::<f64>() { serde_json::json!(num) }
            else if let Ok(bool_val) = value.parse::<bool>() { serde_json::json!(bool_val) }
            else { serde_json::Value::String(value) }
        };
        if let Some(existing) = map.get_mut(&clean_key) {
            if let Some(arr) = existing.as_array_mut() { arr.push(parsed_value); }
            else { let old_val = existing.clone(); *existing = serde_json::json!([old_val, parsed_value]); }
        } else { map.insert(clean_key, parsed_value); }
    }
    for (k, v) in map { context.insert(k, &v); }
    context
}

async fn render_and_generate_static(state: Arc<AppState>, template: &str, context: &tera::Context) -> anyhow::Result<DynamicImage> {
    let json_str = state.template_engine.render(template, context)?;
    let req: StaticMapRequest = serde_json::from_str(&json_str)?;
    generate_static_map_image(state, &req).await
}

async fn render_and_generate_multi(state: Arc<AppState>, template: &str, context: &tera::Context) -> anyhow::Result<DynamicImage> {
    let json_str = state.template_engine.render(template, context)?;
    let req: MultiStaticMapRequest = serde_json::from_str(&json_str)?;
    generate_multi_map_image(state, &req).await
}

// --- Core Logic ---

async fn generate_static_map_image(state: Arc<AppState>, params: &StaticMapRequest) -> anyhow::Result<DynamicImage> {
    let mut marker_data = HashMap::new();
    if let Some(markers) = &params.markers {
        for marker in markers {
            if !marker_data.contains_key(&marker.url) {
                match fetch_image(&state.http_client, &marker.url).await {
                    Ok(img) => {
                        let mut bytes = Vec::new();
                        let mut cursor = Cursor::new(&mut bytes);
                        img.write_to(&mut cursor, image::ImageFormat::Png)?;
                        marker_data.insert(marker.url.clone(), bytes);
                    },
                    Err(e) => error!("failed to fetch marker {}: {}", marker.url, e),
                }
            }
        }
    }

    let scale = params.scale.unwrap_or(1);
    let style_json = if state.config.tile_sources.contains_key(&params.style) {
        json!({
            "version": 8,
            "sources": {
                "raster-tiles": {
                    "type": "raster",
                    "tiles": [ format!("http://localhost:3001/tiles/{}/{{z}}/{{x}}/{{y}}?scale={}", params.style, scale) ],
                    "tileSize": 256
                }
            },
            "layers": [{
                "id": "simple-tiles",
                "type": "raster",
                "source": "raster-tiles",
                "minzoom": 0,
                "maxzoom": 22
            }]
        })
    } else {
        let style_url = format!("{}/style/{}.json", state.config.martin_url, params.style);
        let resp = state.http_client.get(&style_url).send().await?;
        
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("failed to fetch style from {}: {}", style_url, resp.status()));
        }
        let mut sj: serde_json::Value = resp.json().await?;
        rewrite_urls_recursive(&mut sj, &state.config.martin_url);
        sj
    };

    // Acquire permit from semaphore (high concurrency allowed now)
    let _permit = state.render_semaphore.acquire().await.map_err(|e| anyhow::anyhow!("semaphore error: {}", e))?;

    let payload = WorkerPayload {
        request: params.clone(),
        style_json,
        marker_data,
    };

    let exe_path = std::env::current_exe()?;
    let mut child = tokio::process::Command::new(exe_path)
        .arg("--worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("failed to open child stdin"))?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("failed to open child stdout"))?;

    let payload_json = serde_json::to_vec(&payload)?;
    
    use tokio::io::{AsyncWriteExt, AsyncReadExt};

    stdin.write_all(&payload_json).await?;
    drop(stdin); // Close stdin to signal EOF to child

    let mut output_bytes = Vec::new();
    stdout.read_to_end(&mut output_bytes).await?;
    
    let status = child.wait().await?;

    if !status.success() {
        // Try to read stderr
        if let Some(mut stderr) = child.stderr.take() {
            let mut err_msg = String::new();
            let _ = stderr.read_to_string(&mut err_msg).await;
            error!("Worker process failed: {}", err_msg);
            return Err(anyhow::anyhow!("Worker process failed: {}", err_msg));
        }
        return Err(anyhow::anyhow!("Worker process failed with status {}", status));
    }

    if output_bytes.is_empty() {
        return Err(anyhow::anyhow!("Worker returned empty response"));
    }

    let img = image::load_from_memory(&output_bytes)?;
    Ok(img)
}

async fn generate_multi_map_image(state: Arc<AppState>, req: &MultiStaticMapRequest) -> anyhow::Result<DynamicImage> {
    let mut composed_rows = Vec::new();
    for row in &req.grid {
        let mut row_img: Option<DynamicImage> = None;
        for cell in &row.maps {
            let img = generate_static_map_image(state.clone(), &cell.map).await?;
            if let Some(ri) = row_img {
                match cell.direction.as_str() {
                    "right" => {
                        let new_w = ri.width() + img.width();
                        let new_h = ri.height().max(img.height());
                        let mut new_img = DynamicImage::new_rgba8(new_w, new_h);
                        image::imageops::overlay(&mut new_img, &ri, 0, 0);
                        image::imageops::overlay(&mut new_img, &img, ri.width().into(), 0);
                        row_img = Some(new_img);
                    },
                    "bottom" => {
                        let new_w = ri.width().max(img.width());
                        let new_h = ri.height() + img.height();
                        let mut new_img = DynamicImage::new_rgba8(new_w, new_h);
                        image::imageops::overlay(&mut new_img, &ri, 0, 0);
                        image::imageops::overlay(&mut new_img, &img, 0, ri.height().into());
                        row_img = Some(new_img);
                    },
                    _ => row_img = Some(img),
                }
            } else { row_img = Some(img); }
        }
        if let Some(ri) = row_img { composed_rows.push((row.direction.clone(), ri)); }
    }
    
    if composed_rows.is_empty() { return Err(anyhow::anyhow!("no maps to render")); }
    let mut final_img = composed_rows[0].1.clone();
    for (dir, img) in composed_rows.iter().skip(1) {
         match dir.as_str() {
             "right" => {
                let new_w = final_img.width() + img.width();
                let new_h = final_img.height().max(img.height());
                let mut new_img = DynamicImage::new_rgba8(new_w, new_h);
                image::imageops::overlay(&mut new_img, &final_img, 0, 0);
                image::imageops::overlay(&mut new_img, img, final_img.width().into(), 0);
                final_img = new_img;
             },
             "bottom" => {
                let new_w = final_img.width().max(img.width());
                let new_h = final_img.height() + img.height();
                let mut new_img = DynamicImage::new_rgba8(new_w, new_h);
                image::imageops::overlay(&mut new_img, &final_img, 0, 0);
                image::imageops::overlay(&mut new_img, img, 0, final_img.height().into());
                final_img = new_img;
             },
             _ => {}
         }
    }
    Ok(final_img)
}

fn render_map_internal(params: &StaticMapRequest, marker_images: HashMap<String, DynamicImage>, mut style_json: serde_json::Value) -> anyhow::Result<DynamicImage> {
    let scale = params.scale.unwrap_or(1);
    let width = NonZeroU32::new(params.width).ok_or_else(|| anyhow::anyhow!("width must be greater than 0"))?;
    let height = NonZeroU32::new(params.height).ok_or_else(|| anyhow::anyhow!("height must be greater than 0"))?;
    
    let builder = ImageRendererBuilder::new()
        .with_size(width, height)
        .with_pixel_ratio(scale as f32);
    
    let mut renderer = builder.build_static_renderer();
    
    let mut marker_temp_files = Vec::new();
    let mut marker_url_map = HashMap::new();
    for (url, img) in &marker_images {
        let tmp = tempfile::Builder::new().suffix(".png").tempfile()?;
        img.save(tmp.path())?;
        let path_url = Url::from_file_path(tmp.path())
            .map_err(|_| anyhow::anyhow!("failed to create URL from temp file path"))?;
        marker_url_map.insert(url.clone(), path_url.to_string());
        marker_temp_files.push(tmp);
    }

    inject_overlays_into_style(&mut style_json, params, &marker_images, &marker_url_map)?;
    let mut temp_file = tempfile::Builder::new().suffix(".json").tempfile()?;
    temp_file.write_all(serde_json::to_string(&style_json)?.as_bytes())?;
    
    let style_url = Url::from_file_path(temp_file.path())
        .map_err(|_| anyhow::anyhow!("failed to create URL from style temp file path"))?;

    debug!("rendering static map for {} at {},{}", params.style, params.latitude, params.longitude);

    let render_result = std::panic::catch_unwind(move || {
        renderer.load_style_from_url(&style_url);
        renderer.render_static(params.latitude, params.longitude, params.zoom, params.bearing.unwrap_or(0.0), params.pitch.unwrap_or(0.0))
    });

    match render_result {
        Ok(Ok(res)) => Ok(DynamicImage::ImageRgba8(res.as_image().clone())),
        Ok(Err(e)) => Err(anyhow::anyhow!("MapLibre error: {}", e)),
        Err(_) => Err(anyhow::anyhow!("MapLibre C++ panic/abort during rendering")),
    }
}

fn rewrite_urls_recursive(value: &mut serde_json::Value, martin_url: &str) {
    let martin_url = martin_url.trim_end_matches('/');
    match value {
        serde_json::Value::String(s) => {
            if s.contains("http://localhost:3000") || s.contains("http://127.0.0.1:3000") || s.contains("{base}") {
                *s = s.replace("http://localhost:3000", martin_url)
                    .replace("http://127.0.0.1:3000", martin_url)
                    .replace("{base}", martin_url);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr { rewrite_urls_recursive(v, martin_url); }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values_mut() { rewrite_urls_recursive(v, martin_url); }
        }
        _ => {}
    }
}

fn inject_overlays_into_style(
    style: &mut serde_json::Value, 
    params: &StaticMapRequest,
    marker_images: &HashMap<String, DynamicImage>,
    marker_url_map: &HashMap<String, String>
) -> anyhow::Result<()> {
    let style_obj = style.as_object_mut().ok_or_else(|| anyhow::anyhow!("invalid style object"))?;
    if !style_obj.contains_key("sources") { style_obj.insert("sources".to_string(), json!({})); }
    if !style_obj.contains_key("layers") { style_obj.insert("layers".to_string(), json!([])); }
    let mut new_sources = HashMap::new();
    let mut new_layers = Vec::new();

    if let Some(polys) = &params.polygons {
        for (i, poly) in polys.iter().enumerate() {
            let source_id = format!("poly_source_{}", i);
            let mut coordinates = poly.path.iter().map(|(lat, lon)| vec![*lon, *lat]).collect::<Vec<_>>();
            if !coordinates.is_empty() && coordinates[0] != coordinates[coordinates.len() - 1] {
                coordinates.push(coordinates[0].clone());
            }
            new_sources.insert(source_id.clone(), json!({ "type": "geojson", "data": { "type": "Feature", "geometry": { "type": "Polygon", "coordinates": [coordinates] } } }));
            if let Some(color) = &poly.fill_color {
                new_layers.push(json!({ "id": format!("poly_fill_{}", i), "type": "fill", "source": source_id, "paint": { "fill-color": color, "fill-opacity": 1.0 } }));
            }
            if let Some(color) = &poly.stroke_color {
                new_layers.push(json!({ "id": format!("poly_stroke_{}", i), "type": "line", "source": source_id, "paint": { "line-color": color, "line-width": poly.stroke_width.unwrap_or(1) } }));
            }
        }
    }

    if let Some(circles) = &params.circles {
        for (i, circle) in circles.iter().enumerate() {
            let source_id = format!("circle_source_{}", i);
            let mut coordinates = vec![];
            let segments = 64;
            let earth_circumference = 40075016.686;
            let meters_per_degree_lat = earth_circumference / 360.0;
            let meters_per_degree_lon = meters_per_degree_lat * circle.latitude.to_radians().cos();
            for j in 0..segments {
                let angle = (j as f64) * 2.0 * std::f64::consts::PI / (segments as f64);
                let dx = circle.radius * angle.cos();
                let dy = circle.radius * angle.sin();
                coordinates.push(vec![circle.longitude + (dx / meters_per_degree_lon), circle.latitude + (dy / meters_per_degree_lat)]);
            }
            if !coordinates.is_empty() { coordinates.push(coordinates[0].clone()); }
            new_sources.insert(source_id.clone(), json!({ "type": "geojson", "data": { "type": "Feature", "geometry": { "type": "Polygon", "coordinates": [coordinates] } } }));
            if let Some(color) = &circle.fill_color {
                new_layers.push(json!({ "id": format!("circle_fill_{}", i), "type": "fill", "source": source_id, "paint": { "fill-color": color, "fill-opacity": 1.0 } }));
            }
            if let Some(color) = &circle.stroke_color {
                new_layers.push(json!({ "id": format!("circle_stroke_{}", i), "type": "line", "source": source_id, "paint": { "line-color": color, "line-width": circle.stroke_width.unwrap_or(1) } }));
            }
        }
    }

    if let Some(markers) = &params.markers {
        for (i, marker) in markers.iter().enumerate() {
            let source_id = format!("marker_source_{}", i);
            let file_url = if let Some(local_url) = marker_url_map.get(&marker.url) { local_url.clone() } else { continue; };
            let earth_circumference = 40075016.686;
            let meters_per_degree_lat = earth_circumference / 360.0;
            let meters_per_degree_lon = meters_per_degree_lat * marker.latitude.to_radians().cos();
            let meters_per_pixel = (earth_circumference * marker.latitude.to_radians().cos()) / (512.0 * 2.0_f64.powf(params.zoom));
            let (img_w, img_h) = marker_images.get(&marker.url).map(|img| (img.width() as f64, img.height() as f64)).unwrap_or((32.0, 32.0));
            let target_w = marker.width.map(|w| w as f64).unwrap_or(img_w);
            let target_h = marker.height.map(|h| h as f64).unwrap_or(img_h);
            let half_w_m = (target_w * meters_per_pixel) / 2.0;
            let half_h_m = (target_h * meters_per_pixel) / 2.0;
            let d_lat = half_h_m / meters_per_degree_lat;
            let d_lon = half_w_m / meters_per_degree_lon;
            let lat = marker.latitude - (marker.y_offset.unwrap_or(0) as f64 * meters_per_pixel / meters_per_degree_lat);
            let lon = marker.longitude + (marker.x_offset.unwrap_or(0) as f64 * meters_per_pixel / meters_per_degree_lon);
            new_sources.insert(source_id.clone(), json!({ "type": "image", "url": file_url, "coordinates": [[lon - d_lon, lat + d_lat], [lon + d_lon, lat + d_lat], [lon + d_lon, lat - d_lat], [lon - d_lon, lat - d_lat]] }));
            new_layers.push(json!({ "id": format!("marker_layer_{}", i), "type": "raster", "source": source_id, "paint": { "raster-fade-duration": 0 } }));
        }
    }

    let s_obj = style_obj.get_mut("sources").unwrap().as_object_mut().unwrap();
    for (k, v) in new_sources { s_obj.insert(k, v); }
    let l_arr = style_obj.get_mut("layers").unwrap().as_array_mut().unwrap();
    for layer in new_layers { l_arr.push(layer); }
    Ok(())
}