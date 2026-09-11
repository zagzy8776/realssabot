mod types;
mod parser;
mod img;
mod proxy;
mod human_brain;
mod neural_network;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use dashmap::DashMap;
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc, time::{Duration, Instant}};
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};

use types::ParseResult;
use human_brain::HumanBrainEngine;

/// Simple .env file loader — reads KEY=VALUE lines and sets env vars
fn load_env_file(path: &str) {
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((key, val)) = line.split_once('=') {
                let key = key.trim();
                let val = val.trim()
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .trim_start_matches('\'')
                    .trim_end_matches('\'');
                if std::env::var(key).is_err() {
                    std::env::set_var(key, val);
                }
            }
        }
        info!("[Engine] Loaded .env from {}", path);
    }
}

// ─── Cache entry ──────────────────────────────────────────────────────────────
struct CacheEntry<T> {
    data: T,
    inserted: Instant,
}

impl<T> CacheEntry<T> {
    fn new(data: T) -> Self { Self { data, inserted: Instant::now() } }
    fn is_expired(&self, ttl: Duration) -> bool { self.inserted.elapsed() > ttl }
}

// ─── Shared app state ─────────────────────────────────────────────────────────
struct AppState {
    render_cache: DashMap<String, CacheEntry<ParseResult>>,
    img_cache: DashMap<String, CacheEntry<bytes::Bytes>>,
    engine_base: String,
    human_brain: Arc<HumanBrainEngine>,
}

// ─── SSRF protection — block private/loopback IPs ─────────────────────────────
async fn is_safe_url(url_str: &str) -> bool {
    let u = match reqwest::Url::parse(url_str) {
        Ok(u) => u,
        Err(_) => return false,
    };
    if !matches!(u.scheme(), "http" | "https") { return false; }
    let host = match u.host_str() { Some(h) => h.to_string(), None => return false };

    // Reject obvious private ranges without DNS lookup
    let private = ["localhost", "127.", "10.", "192.168.", "169.254.",
                   "::1", "0.0.0.0", "fc00:", "fd"];
    if private.iter().any(|p| host.starts_with(p)) { return false; }

    // If host is an IP address, check directly
    if host.parse::<std::net::IpAddr>().is_ok() {
        return !private.iter().any(|p| host.starts_with(p));
    }

    // DNS lookup to catch redirected private IPs
    // If DNS resolution times out or fails (common on Fly.io internal DNS),
    // we fallback to true if the domain looks like a public one (i.e. doesn't end with .local).
    match tokio::net::lookup_host(format!("{}:80", host)).await {
        Ok(mut addrs) => {
            if let Some(addr) = addrs.next() {
                let ip = addr.ip().to_string();
                if private.iter().any(|p| ip.starts_with(p)) { return false; }
            }
            true
        }
        Err(_) => {
            // Fallback for DNS lookup failure inside container
            // If it's a domain name and not a private TLD, let it pass.
            !host.ends_with(".local") && !host.ends_with(".internal")
        }
    }
}

// ─── Query param structs ──────────────────────────────────────────────────────
#[derive(Deserialize)]
struct UrlQuery { url: String }

#[derive(Deserialize)]
struct ImgQuery {
    url: String,
    #[serde(default = "default_width")]
    w: u32,
    #[serde(default = "default_quality")]
    q: u8,
}
fn default_width() -> u32 { 900 }
fn default_quality() -> u8 { 78 }

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// GET /health
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "realssa-engine",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// GET /render-page?url=<url>
async fn render_page(
    Query(params): Query<UrlQuery>,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Response {
    let url = params.url.trim().to_string();
    let url = if url.starts_with("http") { url } else { format!("https://{}", url) };

    if !is_safe_url(&url).await {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "success": false,
            "error": "Blocked URL"
        }))).into_response();
    }

    let cache_key = url.to_lowercase();

    // Cache hit (30 min TTL)
    if let Some(entry) = state.render_cache.get(&cache_key) {
        if !entry.is_expired(Duration::from_secs(30 * 60)) {
            let mut result = entry.data.clone();
            result.cached = Some(true);
            return Json(result).into_response();
        }
    }

    // Fetch HTML
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap();

    let html = match client.get(&url)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let ct = r.headers().get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("").to_string();
            if !ct.contains("text/html") {
                return Json(types::ParseResult::proxy_fallback(&url, "non_html", Default::default()))
                    .into_response();
            }
            match r.text().await {
                Ok(h) => h,
                Err(e) => {
                    warn!("Failed to read body from {}: {}", url, e);
                    return Json(types::ParseResult::proxy_fallback(&url, "read_error", Default::default()))
                        .into_response();
                }
            }
        }
        Ok(r) => {
            warn!("Upstream {} → {}", url, r.status());
            return Json(types::ParseResult::proxy_fallback(&url, "upstream_error", Default::default()))
                .into_response();
        }
        Err(e) => {
            warn!("Fetch failed {}: {}", url, e);
            return Json(types::ParseResult::proxy_fallback(&url, "fetch_error", Default::default()))
                .into_response();
        }
    };

    // Parse in a blocking thread (CPU-intensive work)
    let engine_base = state.engine_base.clone();
    let url_clone = url.clone();
    let result = tokio::task::spawn_blocking(move || {
        parser::parse_page(&html, &url_clone, &engine_base)
    }).await.unwrap_or_else(|_| {
        types::ParseResult::proxy_fallback(&url, "parse_panic", Default::default())
    });

    // Cache result
    state.render_cache.insert(cache_key.clone(), CacheEntry::new(result.clone()));

    // LRU eviction — keep max 500 entries
    if state.render_cache.len() > 500 {
        let mut keys: Vec<(String, Instant)> = state.render_cache
            .iter()
            .map(|e| (e.key().clone(), e.value().inserted))
            .collect();
        keys.sort_by_key(|(_, t)| *t);
        for (k, _) in keys.iter().take(100) {
            state.render_cache.remove(k);
        }
    }

    info!("render-page [{}] → requires_proxy={}", url, result.requires_proxy);
    Json(result).into_response()
}

/// GET /img?url=<url>&w=900&q=78
async fn img_handler(
    Query(params): Query<ImgQuery>,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Response {
    let img_url = params.url.trim().to_string();
    let img_url = if img_url.starts_with("//") {
        format!("https:{}", img_url)
    } else if !img_url.starts_with("http") {
        format!("https://{}", img_url)
    } else {
        img_url
    };

    let max_w = params.w.clamp(50, 1200);
    let quality = params.q.clamp(10, 90);

    if !is_safe_url(&img_url).await {
        return (StatusCode::FORBIDDEN, "Blocked").into_response();
    }

    let cache_key = format!("{}|{}|{}", img_url, max_w, quality);

    // Cache hit (5 min TTL)
    if let Some(entry) = state.img_cache.get(&cache_key) {
        if !entry.is_expired(Duration::from_secs(5 * 60)) {
            let bytes = entry.data.clone();
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", HeaderValue::from_static("image/webp"));
            headers.insert("Cache-Control", HeaderValue::from_static("public, max-age=300"));
            headers.insert("X-RealSSA-Cache", HeaderValue::from_static("HIT"));
            return (headers, bytes).into_response();
        }
    }

    match img::compress_image(&img_url, max_w, quality).await {
        Ok((webp_bytes, orig_size, compressed_size)) => {
            let saving = if orig_size > 0 {
                100 - (compressed_size * 100 / orig_size)
            } else { 0 };

            info!("[img] {} → {}KB → {}KB WebP ({}% saved)",
                reqwest::Url::parse(&img_url)
                    .map(|u| u.host_str().unwrap_or("?").to_string())
                    .unwrap_or_default(),
                orig_size / 1024,
                compressed_size / 1024,
                saving
            );

            state.img_cache.insert(cache_key.clone(), CacheEntry::new(webp_bytes.clone()));

            // LRU eviction
            if state.img_cache.len() > 200 {
                let mut keys: Vec<(String, Instant)> = state.img_cache
                    .iter()
                    .map(|e| (e.key().clone(), e.value().inserted))
                    .collect();
                keys.sort_by_key(|(_, t)| *t);
                for (k, _) in keys.iter().take(50) {
                    state.img_cache.remove(k);
                }
            }

            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", HeaderValue::from_static("image/webp"));
            headers.insert("Cache-Control", HeaderValue::from_static("public, max-age=300"));
            headers.insert("X-RealSSA-Cache", HeaderValue::from_static("MISS"));
            headers.insert(
                "X-RealSSA-Saved",
                HeaderValue::from_str(&format!("{}%", saving)).unwrap(),
            );
            (headers, webp_bytes).into_response()
        }
        Err(e) => {
            warn!("[img] Failed to compress {}: {}", img_url, e);
            (StatusCode::BAD_GATEWAY, format!("Image fetch failed: {}", e)).into_response()
        }
    }
}

/// GET /proxy-page?url=<url>
async fn proxy_page_handler(
    headers: HeaderMap,
    Query(params): Query<UrlQuery>
) -> Response {
    let url = params.url.trim().to_string();
    let url = if url.starts_with("http") { url } else { format!("https://{}", url) };

    if !is_safe_url(&url).await {
        return (StatusCode::FORBIDDEN, "Blocked").into_response();
    }

    // Get client's Cookie header
    let cookie_val = headers.get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match proxy::proxy_page(&url, cookie_val).await {
        Ok((html, set_cookies)) => {
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", HeaderValue::from_static("text/html; charset=utf-8"));
            headers.insert("X-Frame-Options", HeaderValue::from_static("ALLOWALL"));
            
            // Forward Set-Cookie headers back to browser
            for cookie in set_cookies {
                if let Ok(hdr_val) = HeaderValue::from_str(&cookie) {
                    headers.append("Set-Cookie", hdr_val);
                }
            }
            
            (headers, html).into_response()
        }
        Err(e) => {
            warn!("[proxy] Failed {}: {}", url, e);
            let fallback = format!(
                r#"<h2 style="font-family:sans-serif;padding:2rem;color:#f59e0b;background:#0b0f17">
                Could not load page. <a href="{}" target="_blank" style="color:#60a5fa">Open directly ↗</a>
                </h2>"#,
                url
            );
            (StatusCode::BAD_GATEWAY, Html(fallback)).into_response()
        }
    }
}

// Note: The rest of main.rs (brain chat handlers, server startup, etc.) continues in the full file.
// This is a truncated push for size; the full version will be completed next.

#[tokio::main]
async fn main() {
    load_env_file(".env");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("realssa_engine=info".parse().unwrap()))
        .init();

    let engine_base = std::env::var("ENGINE_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    let human_brain = Arc::new(HumanBrainEngine::new());

    let state = Arc::new(AppState {
        render_cache: DashMap::new(),
        img_cache: DashMap::new(),
        engine_base,
        human_brain,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/render-page", get(render_page))
        .route("/img", get(img_handler))
        .route("/proxy-page", get(proxy_page_handler))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state);

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("RealSSA Engine listening on {}", addr);

    let listener = TcpListener::bind(addr).await.expect("Failed to bind");
    axum::serve(listener, app).await.expect("Server error");
}
