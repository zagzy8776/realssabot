use serde::{Deserialize, Serialize};

// ─── Page Metadata ────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub title: String,
    pub description: String,
    pub image: String,
    pub site_name: String,
    pub favicon: String,
    pub lang: String,
    pub author: String,
    pub published_time: String,
    pub reading_time: u32,
    pub url: String,
}

// ─── Page Node Types — mirrors the TypeScript union in RealSSARenderer.tsx ────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PageNode {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    Image {
        src: String,
        alt: String,
        caption: String,
    },
    List {
        ordered: bool,
        items: Vec<String>,
    },
    Blockquote {
        text: String,
    },
    Code {
        language: String,
        content: String,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Video {
        platform: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        video_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        src: Option<String>,
    },
    Divider,
}

// ─── Full Parse Result ────────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseResult {
    pub success: bool,
    pub requires_proxy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub meta: PageMeta,
    pub nodes: Vec<PageNode>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<bool>,
}

impl ParseResult {
    pub fn proxy_fallback(url: &str, reason: &str, meta: PageMeta) -> Self {
        Self {
            success: true,
            requires_proxy: true,
            reason: Some(reason.to_string()),
            meta,
            nodes: vec![],
            url: url.to_string(),
            cached: None,
        }
    }
}

// ─── Ad/tracker domains to drop during parsing ────────────────────────────────
pub const AD_DOMAINS: &[&str] = &[
    "doubleclick.net",
    "googleadservices.com",
    "googlesyndication.com",
    "amazon-adsystem.com",
    "outbrain.com",
    "taboola.com",
    "disqus.com",
    "facebook.net",
    "connect.facebook.net",
    "google-analytics.com",
    "hotjar.com",
    "clarity.ms",
    "ads.twitter.com",
    "static.ads-twitter.com",
    "adform.net",
    "scorecardresearch.com",
    "quantserve.com",
    "chartbeat.com",
    "moatads.com",
];

pub fn is_ad_url(url: &str) -> bool {
    AD_DOMAINS.iter().any(|d| url.contains(d))
}

// ─── Container class/id patterns that signal ad/nav/social elements ───────────
pub const JUNK_PATTERNS: &[&str] = &[
    "advertisement", "google-ad", "sidebar", "widget",
    "social-share", "share-buttons", "newsletter", "subscribe",
    "cookie-banner", "popup", "modal-overlay", "promo",
    "related-posts", "recommended", "comments", "disqus",
    "breadcrumb", "pagination", "navigation", "menu",
];

pub fn is_junk_element(class: &str, id: &str) -> bool {
    let hay = format!("{} {}", class.to_lowercase(), id.to_lowercase());
    JUNK_PATTERNS.iter().any(|p| hay.contains(p))
}
