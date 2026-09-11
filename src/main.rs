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

fn load_env_file(path: &str) {
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((key, val)) = line.split_once('=') {
                let key = key.trim();
                let val = val.trim().trim_start_matches('"').trim_end_matches('"').trim_start_matches('\'').trim_end_matches('\'');
                if std::env::var(key).is_err() { std::env::set_var(key, val); }
            }
        }
        info!("[Engine] Loaded .env from {}", path);
    }
}

struct CacheEntry<T> { data: T, inserted: Instant }
impl<T> CacheEntry<T> {
    fn new(data: T) -> Self { Self { data, inserted: Instant::now() } }
    fn is_expired(&self, ttl: Duration) -> bool { self.inserted.elapsed() > ttl }
}

struct AppState {
    render_cache: DashMap<String, CacheEntry<ParseResult>>,
    img_cache: DashMap<String, CacheEntry<bytes::Bytes>>,
    engine_base: String,
    human_brain: Arc<HumanBrainEngine>,
}

async fn is_safe_url(url_str: &str) -> bool {
    let u = match reqwest::Url::parse(url_str) { Ok(u) => u, Err(_) => return false };
    if !matches!(u.scheme(), "http" | "https") { return false; }
    let host = match u.host_str() { Some(h) => h.to_string(), None => return false };
    let private = ["localhost", "127.", "10.", "192.168.", "169.254.", "::1", "0.0.0.0", "fc00:", "fd"];
    if private.iter().any(|p| host.starts_with(p)) { return false; }
    if host.parse::<std::net::IpAddr>().is_ok() { return !private.iter().any(|p| host.starts_with(p)); }
    match tokio::net::lookup_host(format!("{}:80", host)).await {
        Ok(mut addrs) => { if let Some(addr) = addrs.next() { if private.iter().any(|p| addr.ip().to_string().starts_with(p)) { return false; } } true }
        Err(_) => !host.ends_with(".local") && !host.ends_with(".internal"),
    }
}

#[derive(Deserialize)] struct UrlQuery { url: String }
#[derive(Deserialize)] struct ImgQuery {
    url: String,
    #[serde(default = "default_width")] w: u32,
    #[serde(default = "default_quality")] q: u8,
}
fn default_width() -> u32 { 900 }
fn default_quality() -> u8 { 78 }

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok","service":"realssa-engine","version":env!("CARGO_PKG_VERSION")}))
}

async fn render_page(Query(params): Query<UrlQuery>, axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> Response {
    let url = params.url.trim().to_string();
    let url = if url.starts_with("http") { url } else { format!("https://{}", url) };
    if !is_safe_url(&url).await { return (StatusCode::FORBIDDEN, Json(serde_json::json!({"success":false,"error":"Blocked URL"}))).into_response(); }
    let cache_key = url.to_lowercase();
    if let Some(entry) = state.render_cache.get(&cache_key) {
        if !entry.is_expired(Duration::from_secs(30*60)) { let mut result=entry.data.clone(); result.cached=Some(true); return Json(result).into_response(); }
    }
    let client=reqwest::Client::builder().timeout(Duration::from_secs(12)).user_agent("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)").redirect(reqwest::redirect::Policy::limited(5)).build().unwrap();
    let html=match client.get(&url).header("Accept","text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8").header("Accept-Language","en-US,en;q=0.9").send().await {
        Ok(r) if r.status().is_success() => {
            let ct=r.headers().get("content-type").and_then(|v|v.to_str().ok()).unwrap_or("").to_string();
            if !ct.contains("text/html") { return Json(types::ParseResult::proxy_fallback(&url,"non_html",Default::default())).into_response(); }
            match r.text().await { Ok(h)=>h, Err(e)=>{warn!("Failed to read body from {}: {}",url,e); return Json(types::ParseResult::proxy_fallback(&url,"read_error",Default::default())).into_response();} }
        }
        Ok(r)=>{warn!("Upstream {} → {}",url,r.status()); return Json(types::ParseResult::proxy_fallback(&url,"upstream_error",Default::default())).into_response();}
        Err(e)=>{warn!("Fetch failed {}: {}",url,e); return Json(types::ParseResult::proxy_fallback(&url,"fetch_error",Default::default())).into_response();}
    };
    let engine_base=state.engine_base.clone(); let url_clone=url.clone();
    let result=tokio::task::spawn_blocking(move||parser::parse_page(&html,&url_clone,&engine_base)).await.unwrap_or_else(|_|types::ParseResult::proxy_fallback(&url,"parse_panic",Default::default()));
    state.render_cache.insert(cache_key.clone(),CacheEntry::new(result.clone()));
    if state.render_cache.len()>500 { let mut keys:Vec<(String,Instant)>=state.render_cache.iter().map(|e|(e.key().clone(),e.value().inserted)).collect(); keys.sort_by_key(|(_,t)|*t); for (k,_) in keys.iter().take(100){state.render_cache.remove(k);} }
    info!("render-page [{}] → requires_proxy={}",url,result.requires_proxy); Json(result).into_response()
}

async fn img_handler(Query(params): Query<ImgQuery>, axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> Response {
    let img_url=params.url.trim().to_string();
    let img_url=if img_url.starts_with("//"){format!("https:{}",img_url)}else if !img_url.starts_with("http"){format!("https://{}",img_url)}else{img_url};
    let max_w=params.w.clamp(50,1200); let quality=params.q.clamp(10,90);
    if !is_safe_url(&img_url).await{return(StatusCode::FORBIDDEN,"Blocked").into_response();}
    let cache_key=format!("{}|{}|{}",img_url,max_w,quality);
    if let Some(entry)=state.img_cache.get(&cache_key){if !entry.is_expired(Duration::from_secs(5*60)){let bytes=entry.data.clone();let mut headers=HeaderMap::new();headers.insert("Content-Type",HeaderValue::from_static("image/webp"));headers.insert("Cache-Control",HeaderValue::from_static("public, max-age=300"));headers.insert("X-RealSSA-Cache",HeaderValue::from_static("HIT"));return(headers,bytes).into_response();}}
    match img::compress_image(&img_url,max_w,quality).await {
        Ok((webp_bytes,orig_size,compressed_size))=>{let saving=if orig_size>0{100-(compressed_size*100/orig_size)}else{0};info!("[img] {} → {}KB → {}KB WebP ({}% saved)",reqwest::Url::parse(&img_url).map(|u|u.host_str().unwrap_or("?").to_string()).unwrap_or_default(),orig_size/1024,compressed_size/1024,saving);state.img_cache.insert(cache_key.clone(),CacheEntry::new(webp_bytes.clone()));if state.img_cache.len()>200{let mut keys:Vec<(String,Instant)>=state.img_cache.iter().map(|e|(e.key().clone(),e.value().inserted)).collect();keys.sort_by_key(|(_,t)|*t);for(k,_)in keys.iter().take(50){state.img_cache.remove(k);}}let mut headers=HeaderMap::new();headers.insert("Content-Type",HeaderValue::from_static("image/webp"));headers.insert("Cache-Control",HeaderValue::from_static("public, max-age=300"));headers.insert("X-RealSSA-Cache",HeaderValue::from_static("MISS"));headers.insert("X-RealSSA-Saved",HeaderValue::from_str(&format!("{}%",saving)).unwrap());(headers,webp_bytes).into_response()}
        Err(e)=>{warn!("[img] Failed to compress {}: {}",img_url,e);(StatusCode::BAD_GATEWAY,format!("Image fetch failed: {}",e)).into_response()}
    }
}

async fn proxy_page_handler(headers:HeaderMap,Query(params):Query<UrlQuery>)->Response{
    let url=params.url.trim().to_string();let url=if url.starts_with("http"){url}else{format!("https://{}",url)};if !is_safe_url(&url).await{return(StatusCode::FORBIDDEN,"Blocked").into_response();}
    let cookie_val=headers.get("cookie").and_then(|v|v.to_str().ok()).unwrap_or("");
    match proxy::proxy_page(&url,cookie_val).await{Ok((html,set_cookies))=>{let mut headers=HeaderMap::new();headers.insert("Content-Type",HeaderValue::from_static("text/html; charset=utf-8"));headers.insert("X-Frame-Options",HeaderValue::from_static("ALLOWALL"));for cookie in set_cookies{if let Ok(v)=HeaderValue::from_str(&cookie){headers.append("Set-Cookie",v);}}(headers,html).into_response()}
    Err(e)=>{warn!("[proxy] Failed {}: {}",url,e);let fallback=format!(r#"<h2 style="font-family:sans-serif;padding:2rem;color:#f59e0b;background:#0b0f17">Could not load page. <a href="{}" target="_blank" style="color:#60a5fa">Open directly ↗</a></h2>"#,url);(StatusCode::BAD_GATEWAY,Html(fallback)).into_response()}}
}

#[derive(Deserialize)] struct ChatRequest{message:String}
#[derive(Deserialize)] struct RlhfRequest{category:String,phrase:String,reward:f32}
#[derive(Deserialize)] struct ConvPairRequest{question:String,answer:String}
#[derive(Deserialize)] struct TeachRequest{category:String,phrase:String,#[serde(default)]context:String,#[serde(default)]nuance:String}

async fn human_brain_insights_handler(State(state):State<Arc<AppState>>)->impl IntoResponse{let memory=state.human_brain.get_formatted_human_context(15).await;let(total_insights,occurrences)=state.human_brain.get_stats().await;Json(serde_json::json!({"success":true,"memory":memory,"total_insights":total_insights,"total_occurrences":occurrences}))}
async fn human_brain_chat_handler(State(state):State<Arc<AppState>>,Json(payload):Json<ChatRequest>)->impl IntoResponse{let(reply,memory_used)=state.human_brain.chat(&payload.message).await;let(total_insights,occurrences)=state.human_brain.get_stats().await;Json(serde_json::json!({"success":true,"reply":reply,"memory_used":memory_used,"total_insights":total_insights,"total_occurrences":occurrences}))}
async fn human_brain_native_chat_handler(State(state):State<Arc<AppState>>,Json(payload):Json<ChatRequest>)->impl IntoResponse{let(reply,vector_score)=state.human_brain.native_rust_inference(&payload.message).await;Json(serde_json::json!({"success":true,"engine":"Native Rust Vector Neural Engine","reply":reply,"vector_score":vector_score}))}
async fn human_brain_rlhf_handler(State(state):State<Arc<AppState>>,Json(payload):Json<RlhfRequest>)->impl IntoResponse{state.human_brain.apply_rlhf_feedback(&payload.category,&payload.phrase,payload.reward).await;Json(serde_json::json!({"success":true,"message":"RLHF reward signal applied & saved to Rust persistent storage","category":payload.category,"phrase":payload.phrase,"new_reward_signal":payload.reward}))}
async fn human_brain_conv_pair_handler(State(state):State<Arc<AppState>>,Json(payload):Json<ConvPairRequest>)->impl IntoResponse{state.human_brain.record_conv_pair(&payload.question,&payload.answer).await;let(total_insights,_)=state.human_brain.get_stats().await;Json(serde_json::json!({"success":true,"message":"Conversation pair trained & saved","question":payload.question,"answer":payload.answer,"total_insights":total_insights}))}
async fn human_brain_teach_handler(State(state):State<Arc<AppState>>,Json(payload):Json<TeachRequest>)->impl IntoResponse{state.human_brain.record_insight(&payload.category,&payload.phrase,&payload.context,&payload.nuance).await;let(total_insights,occurrences)=state.human_brain.get_stats().await;Json(serde_json::json!({"success":true,"message":"Knowledge taught & saved to Rust persistent memory","category":payload.category,"phrase":payload.phrase,"total_insights":total_insights,"total_occurrences":occurrences}))}

#[tokio::main]
async fn main(){
    load_env_file("../.env");load_env_file(".env");load_env_file("../../.env");
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_|"realssa_engine=info,tower_http=warn".to_string())).init();
    let port:u16=std::env::var("PORT").ok().and_then(|p|p.parse().ok()).unwrap_or(8080);
    let engine_base=std::env::var("ENGINE_BASE_URL").unwrap_or_else(|_|format!("http://localhost:{}",port));
    info!("RealSSA Engine starting on port {}, engine_base={}",port,engine_base);
    let brain=Arc::new(HumanBrainEngine::new());brain.clone().start_silent_worker();seed_conv_pairs(brain.clone());
    let state=Arc::new(AppState{render_cache:DashMap::new(),img_cache:DashMap::new(),engine_base,human_brain:brain});
    let cors=CorsLayer::new().allow_origin(["https://www.realssanews.com.ng".parse().unwrap(),"https://realssanews.com.ng".parse().unwrap(),"http://localhost:5173".parse().unwrap(),"http://localhost:3000".parse().unwrap()]).allow_methods([axum::http::Method::GET,axum::http::Method::POST,axum::http::Method::OPTIONS]).allow_headers(Any);
    let app=Router::new().route("/health",get(health)).route("/render-page",get(render_page)).route("/img",get(img_handler)).route("/proxy-page",get(proxy_page_handler)).route("/human-brain/insights",get(human_brain_insights_handler)).route("/human-brain/chat",post(human_brain_chat_handler)).route("/human-brain/native-chat",post(human_brain_native_chat_handler)).route("/human-brain/rlhf",post(human_brain_rlhf_handler)).route("/human-brain/teach",post(human_brain_teach_handler)).route("/human-brain/conv-pair",post(human_brain_conv_pair_handler)).layer(cors).with_state(state);
    let addr=SocketAddr::from(([0,0,0,0],port));info!("Listening on {}",addr);let listener=TcpListener::bind(addr).await.unwrap();axum::serve(listener,app).with_graceful_shutdown(shutdown_signal()).await.unwrap();
}

fn seed_conv_pairs(brain:Arc<HumanBrainEngine>){tokio::spawn(async move{tokio::time::sleep(Duration::from_secs(8)).await;let pairs:&[(&str,&str)]=&[
("who are you","I'm RealSSA — a self-learning Nigerian news AI built entirely in Rust. I cover politics, sports, and tech across Africa."),
("what are you","I'm RealSSA, a distributed Rust AI engine with 6 Neon vector databases. I learn silently from Nigerian and African news sources every few minutes."),
("who built you","I was built by the RealSSA team — a Nigerian news platform powered by a custom Rust neural engine with real-time web crawling."),
("what can you do","I can answer questions about Nigerian news, African politics, tech, and sports. I also learn continuously from live RSS feeds across Nigeria."),
("how far bro","I dey o! Everything is running smooth. Ask me anything about Nigeria, Africa, tech, or sports."),
("wetin dey happen","Plenty things dey happen! From Nigerian politics to African tech — just ask me anything specific and I go break am down for you."),
("how are you","I'm doing great! My Rust engine is fully active and learning. What can I help you with today?"),
("what is happening in Nigeria","Nigeria is always buzzing — from NASS debates to economic policy, AFCON qualifiers, and Lagos tech startups. Ask me about a specific topic for the latest."),
("tell me about african tech","African tech is booming! Lagos, Nairobi, Accra, and Johannesburg are leading fintech, agritech, and healthtech innovation. Nigeria alone has produced Flutterwave, Paystack, and Andela."),
("latest nigeria news","I crawl Nigerian news sources like Vanguard, Punch, Guardian, and Premium Times every 6 minutes. Ask me about a specific topic — politics, economy, sports, or tech."),
("tell me about realssa","RealSSA is a Nigerian news platform that delivers real-time news, live sports scores, and AI-powered summaries. I'm the Rust brain behind it."),
("what is the economy like in nigeria","Nigeria's economy faces inflation and FX pressure, but sectors like fintech, oil & gas, and agriculture remain strong drivers of growth."),
("who is the president of nigeria","Bola Ahmed Tinubu is the President of Nigeria, having taken office on May 29, 2023."),
("tell me about nigerian football","Nigerian football is passionate! The Super Eagles compete in AFCON and World Cup qualifiers. Domestically, the NPFL has clubs like Enyimba, Rivers United, and Remo Stars."),
("what is afcon","AFCON is the Africa Cup of Nations — the biggest football tournament on the continent, organised by CAF. Nigeria has won it three times."),
];for(q,a)in pairs{brain.record_conv_pair(q,a).await;}info!("[Boot] Seeded {} core conversation pairs",pairs.len());});}

async fn shutdown_signal(){tokio::signal::ctrl_c().await.expect("Failed to install Ctrl+C handler");info!("Shutdown signal received");}
